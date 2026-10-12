//! The Claude API as a model: one Messages API request with an API key, the
//! schema as a structured output, and no tools.
//!
//! As with the CLI, a message crafted to trick the model has no tools to
//! use: the worst it can do is fill the schema wrongly, which the features
//! check.

use std::time::Duration;

use futures::future::BoxFuture;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::{AiError, Image, Llm, Request, shorten};
use crate::config::{AiConfig, Secret};

pub const DEFAULT_URL: &str = "https://api.anthropic.com";

/// The API version the requests are written for.
const VERSION: &str = "2023-06-01";

/// Room for a long bill's transcript and claims, and the model's thinking.
const MAX_TOKENS: u32 = 16_000;

/// The models `ai.model` may name by their family, as the CLI does.
const ALIASES: &[(&str, &str)] = &[
    ("opus", "claude-opus-5"),
    ("sonnet", "claude-sonnet-5"),
    ("haiku", "claude-haiku-4-5"),
];

/// Models that may decline a request: the API then answers it with another
/// model by itself.
const FALLBACK_MODELS: &[&str] = &["claude-opus-5", "claude-fable-5-1"];
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

pub struct AnthropicApi {
    client: reqwest::Client,
    url: String,
    key: Secret<String>,
    permits: Semaphore,
}

impl std::fmt::Debug for AnthropicApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicApi")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl AnthropicApi {
    /// The API configured by `[ai]`, if a key is.
    pub fn from_config(config: &AiConfig) -> Option<Self> {
        let key = config.api_key()?.clone();
        let url = config.api_url.as_deref().unwrap_or(DEFAULT_URL);
        let timeout = Duration::from_secs(config.timeout_secs);
        match Self::new(url, key, timeout, config.max_concurrent) {
            Ok(api) => Some(api),
            Err(error) => {
                tracing::error!(%error, "couldn't set up the Claude API's client");
                None
            }
        }
    }

    pub fn new(
        url: &str,
        key: Secret<String>,
        timeout: Duration,
        max_concurrent: usize,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(timeout).build()?,
            url: url.trim_end_matches('/').to_string(),
            key,
            permits: Semaphore::new(max_concurrent.max(1)),
        })
    }

    async fn run(&self, request: &Request) -> Result<Value, AiError> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| AiError::Unavailable("shutting down".into()))?;
        let model = model_id(&request.model);
        let mut call = self
            .client
            .post(format!("{}/v1/messages", self.url))
            .header("x-api-key", self.key.expose())
            .header("anthropic-version", VERSION)
            .json(&body(request, model));
        if FALLBACK_MODELS.contains(&model) {
            call = call.header("anthropic-beta", FALLBACK_BETA);
        }

        let unreachable = |error: reqwest::Error| {
            if error.is_timeout() {
                AiError::Timeout
            } else {
                AiError::Unavailable(error.to_string())
            }
        };
        let response = call.send().await.map_err(unreachable)?;
        let status = response.status();
        let id = response
            .headers()
            .get("request-id")
            .and_then(|id| id.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let text = response.text().await.map_err(unreachable)?;
        if !status.is_success() {
            tracing::warn!(%status, request_id = id, "the Claude API refused a request");
            return Err(failure(status, &text));
        }
        let message: Message = serde_json::from_str(&text)
            .map_err(|error| AiError::Invalid(format!("unreadable answer: {error}")))?;
        tracing::debug!(
            model,
            request_id = id,
            input_tokens = message.usage.input_tokens,
            output_tokens = message.usage.output_tokens,
            "the Claude API answered"
        );
        message.into_answer()
    }
}

/// The model `name` stands for: a family's current model, or itself.
fn model_id(name: &str) -> &str {
    let name = name.trim();
    ALIASES
        .iter()
        .find(|(alias, _)| name.eq_ignore_ascii_case(alias))
        .map_or(name, |(_, id)| id)
}

/// The Messages API request: the images, then the text, to fill the schema.
fn body(request: &Request, model: &str) -> Value {
    let mut content: Vec<Value> = request.images.iter().map(Image::block).collect();
    content.push(json!({"type": "text", "text": request.text}));
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": request.system,
        "messages": [{"role": "user", "content": content}],
        "output_config": {"format": {"type": "json_schema", "schema": nullable_as_any_of(&request.schema)}},
    });
    if FALLBACK_MODELS.contains(&model) {
        body["fallbacks"] = json!("default");
    }
    body
}

/// `schema` as the API's structured outputs take it: a type that may be null
/// (`"type": ["string", "null"]`, with `null` among its `enum`) becomes
/// `anyOf` the type and null, since the API checks an `enum` against a
/// single type.
fn nullable_as_any_of(schema: &Value) -> Value {
    match schema {
        Value::Array(schemas) => Value::Array(schemas.iter().map(nullable_as_any_of).collect()),
        Value::Object(fields) => {
            // Subschemas first, wherever they are.
            let mut fields: serde_json::Map<String, Value> = fields
                .iter()
                .map(|(key, value)| {
                    let value = match key.as_str() {
                        "properties" | "$defs" | "definitions" => match value {
                            Value::Object(named) => Value::Object(
                                named
                                    .iter()
                                    .map(|(name, schema)| {
                                        (name.clone(), nullable_as_any_of(schema))
                                    })
                                    .collect(),
                            ),
                            other => other.clone(),
                        },
                        "items" | "anyOf" | "allOf" | "oneOf" => nullable_as_any_of(value),
                        _ => value.clone(),
                    };
                    (key.clone(), value)
                })
                .collect();
            let Some(Value::Array(types)) = fields.get("type") else {
                return Value::Object(fields);
            };
            let null = Value::from("null");
            if !types.contains(&null) {
                return Value::Object(fields);
            }
            let others: Vec<Value> = types
                .iter()
                .filter(|kind| **kind != null)
                .cloned()
                .collect();
            fields.insert(
                "type".into(),
                match others.as_slice() {
                    [only] => only.clone(),
                    _ => Value::Array(others),
                },
            );
            if let Some(Value::Array(values)) = fields.get_mut("enum") {
                values.retain(|value| !value.is_null());
            }
            json!({"anyOf": [Value::Object(fields), {"type": "null"}]})
        }
        other => other.clone(),
    }
}

/// What the Messages API answers.
#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Vec<Block>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Usage,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Default, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

impl Message {
    /// The filled schema: the answer's text (thinking aside).
    fn into_answer(self) -> Result<Value, AiError> {
        match self.stop_reason.as_deref() {
            Some("refusal") => {
                return Err(AiError::Failed("the model declined to read it".into()));
            }
            Some("max_tokens") => {
                return Err(AiError::Failed(
                    "the answer was too long, and cut off".into(),
                ));
            }
            _ => {}
        }
        let text: String = self
            .content
            .iter()
            .filter(|block| block.kind == "text")
            .filter_map(|block| block.text.as_deref())
            .collect();
        serde_json::from_str(text.trim())
            .map_err(|_| AiError::Invalid(format!("no structured answer in {}", shorten(&text))))
    }
}

/// Why the API turned a request down, from its status and error message.
fn failure(status: StatusCode, body: &str) -> AiError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|error| error["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string());
    let message = shorten(&message);
    match status.as_u16() {
        401 | 403 => AiError::Unavailable(format!("the API key was refused: {message}")),
        429 => AiError::Unavailable(format!("too many requests: {message}")),
        // Overloaded (529) and other server errors: worth trying again.
        500.. => AiError::Unavailable(message),
        // A bad request, e.g. the credit has run out.
        _ => AiError::Failed(message),
    }
}

impl Llm for AnthropicApi {
    fn complete<'a>(&'a self, request: &'a Request) -> BoxFuture<'a, Result<Value, AiError>> {
        Box::pin(self.run(request))
    }
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    fn request(model: &str, images: Vec<Image>) -> Request {
        Request {
            system: "Extract the answer.".into(),
            text: "the answer is 42".into(),
            schema: json!({"type": "object"}),
            model: model.into(),
            images,
        }
    }

    /// A one-request HTTP server answering `status` and `body`; the handle
    /// gives back the request it got.
    async fn serve(status: u16, body: Value) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buffer = [0; 4096];
            // The headers, then as much body as they announce.
            let complete = |received: &[u8]| {
                let text = String::from_utf8_lossy(received);
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    return false;
                };
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                body.len() >= length
            };
            while !complete(&received) {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                received.extend_from_slice(&buffer[..read]);
            }
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: \
                 {}\r\nrequest-id: req_1\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(received).unwrap()
        });
        (url, handle)
    }

    fn api(url: &str) -> AnthropicApi {
        AnthropicApi::new(
            url,
            Secret::new("sk-test".into()),
            Duration::from_secs(10),
            1,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn the_api_gets_the_request_and_answers_with_the_schema() {
        let answer = json!({
            "content": [
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "{\"answer\": 42}"},
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5},
        });
        let (url, server) = serve(200, answer).await;
        let image = Image {
            media_type: "image/png".into(),
            data: b"hi".to_vec(),
        };
        let filled = api(&url)
            .complete(&request("opus", vec![image]))
            .await
            .unwrap();
        assert_eq!(filled, json!({"answer": 42}));

        let received = server.await.unwrap();
        let (head, body) = received.split_once("\r\n\r\n").unwrap();
        let head = head.to_lowercase();
        assert!(head.starts_with("post /v1/messages "), "{head}");
        for header in [
            "x-api-key: sk-test",
            "anthropic-version: 2023-06-01",
            "anthropic-beta: server-side-fallback-2026-07-01",
        ] {
            assert!(head.contains(header), "{header} in {head}");
        }
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["model"], "claude-opus-5");
        assert_eq!(body["system"], "Extract the answer.");
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(
            body["output_config"]["format"],
            json!({"type": "json_schema", "schema": {"type": "object"}})
        );
        assert_eq!(
            body["messages"][0]["content"],
            json!([
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aGk="}},
                {"type": "text", "text": "the answer is 42"},
            ])
        );
    }

    #[tokio::test]
    async fn refusals_and_refused_requests_are_errors() {
        let refused = json!({"content": [], "stop_reason": "refusal"});
        let (url, _server) = serve(200, refused).await;
        let error = api(&url)
            .complete(&request("sonnet", Vec::new()))
            .await
            .unwrap_err();
        assert!(matches!(error, AiError::Failed(_)), "{error:?}");

        let low = json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": "Your credit balance is too low"},
        });
        let (url, server) = serve(400, low).await;
        let error = api(&url)
            .complete(&request("sonnet", Vec::new()))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "the AI failed: Your credit balance is too low"
        );
        // No fallback for a model that doesn't decline.
        let received = server.await.unwrap().to_lowercase();
        assert!(!received.contains("anthropic-beta"), "{received}");
        assert!(!received.contains("fallbacks"), "{received}");

        let (url, _server) = serve(401, json!({"error": {"message": "invalid x-api-key"}})).await;
        let error = api(&url)
            .complete(&request("haiku", Vec::new()))
            .await
            .unwrap_err();
        assert!(matches!(error, AiError::Unavailable(_)), "{error:?}");
    }

    #[test]
    fn types_that_may_be_null_become_any_of() {
        let schema = json!({
            "type": "object",
            "properties": {
                "category": {"type": ["string", "null"], "enum": ["food", "stay", null]},
                "note": {"type": ["string", "null"]},
                "kind": {"type": "string", "enum": ["expense", "settlement"]},
                "amount": {
                    "type": ["object", "null"],
                    "properties": {"value": {"type": ["string", "null"]}},
                    "required": ["value"],
                    "additionalProperties": false,
                },
                "claims": {"type": "array", "items": {"type": ["string", "null"]}},
            },
            "required": ["category", "note", "kind", "amount", "claims"],
            "additionalProperties": false,
        });
        let text = json!({"anyOf": [{"type": "string"}, {"type": "null"}]});
        assert_eq!(
            nullable_as_any_of(&schema),
            json!({
                "type": "object",
                "properties": {
                    "category": {"anyOf": [
                        {"type": "string", "enum": ["food", "stay"]},
                        {"type": "null"},
                    ]},
                    "note": text,
                    "kind": {"type": "string", "enum": ["expense", "settlement"]},
                    "amount": {"anyOf": [
                        {
                            "type": "object",
                            "properties": {"value": text},
                            "required": ["value"],
                            "additionalProperties": false,
                        },
                        {"type": "null"},
                    ]},
                    "claims": {"type": "array", "items": text},
                },
                "required": ["category", "note", "kind", "amount", "claims"],
                "additionalProperties": false,
            })
        );
    }

    /// Whether `schema` still has a type list anywhere.
    fn has_type_list(schema: &Value) -> bool {
        match schema {
            Value::Object(fields) => {
                fields.get("type").is_some_and(Value::is_array)
                    || fields.values().any(has_type_list)
            }
            Value::Array(values) => values.iter().any(has_type_list),
            _ => false,
        }
    }

    #[test]
    fn the_features_schemas_have_no_type_lists_left() {
        use crate::modules::trips::{ask, extract, model, settings::TripsSettings, story};
        let categories = model::categories(&TripsSettings::default());
        for schema in [
            extract::schema(&categories),
            ask::schema(&categories),
            story::schema(),
        ] {
            assert!(has_type_list(&schema) || schema == story::schema());
            assert!(!has_type_list(&nullable_as_any_of(&schema)), "{schema}");
        }
    }

    #[test]
    fn families_name_their_current_model() {
        assert_eq!(model_id("opus"), "claude-opus-5");
        assert_eq!(model_id(" Sonnet "), "claude-sonnet-5");
        assert_eq!(model_id("haiku"), "claude-haiku-4-5");
        assert_eq!(model_id("claude-opus-5-5"), "claude-opus-5-5");
    }

    /// Spends a little of the real credit: run with `ASSISTANT_AI__API_KEY`
    /// set and `cargo test -- --ignored real_api`.
    #[tokio::test]
    #[ignore = "calls the real API"]
    async fn real_api_fills_a_schema() {
        let key = std::env::var("ASSISTANT_AI__API_KEY").expect("ASSISTANT_AI__API_KEY");
        let api =
            AnthropicApi::new(DEFAULT_URL, Secret::new(key), Duration::from_secs(120), 1).unwrap();
        let mut request = request("haiku", Vec::new());
        request.schema = json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
            "additionalProperties": false,
        });
        let filled = api.complete(&request).await.unwrap();
        assert_eq!(filled, json!({"answer": 42}));
    }
}
