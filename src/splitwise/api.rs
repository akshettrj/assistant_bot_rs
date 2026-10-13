//! Splitwise's API (v3.0) with a personal API key, sent as a bearer token.
//!
//! Requests are JSON, with an expense's shares flattened into
//! `users__<n>__<field>` keys. A `200 OK` isn't enough: an answer with
//! `errors`, or with `success: false`, failed.

use std::time::Duration;

use futures::future::BoxFuture;
use reqwest::{Method, StatusCode};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Category, Expense, Group, Person, Result, Splitwise, SplitwiseError, Stored, User};
use crate::config::{Secret, SplitwiseConfig};

pub const DEFAULT_URL: &str = "https://secure.splitwise.com/api/v3.0";

/// The longest error text kept from an answer.
const MAX_ERROR: usize = 300;

pub struct SplitwiseApi {
    client: reqwest::Client,
    url: String,
    key: Secret<String>,
}

impl std::fmt::Debug for SplitwiseApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SplitwiseApi")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl SplitwiseApi {
    /// The API configured by `[splitwise]`, if a key is.
    pub fn from_config(config: &SplitwiseConfig) -> Option<Self> {
        let key = config.api_key()?.clone();
        let url = config.api_url.as_deref().unwrap_or(DEFAULT_URL);
        match Self::new(url, key, Duration::from_secs(config.timeout_secs)) {
            Ok(api) => Some(api),
            Err(error) => {
                tracing::error!(%error, "couldn't set up the Splitwise client");
                None
            }
        }
    }

    pub fn new(url: &str, key: Secret<String>, timeout: Duration) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(timeout).build()?,
            url: url.trim_end_matches('/').to_string(),
            key,
        })
    }

    /// Calls `path`, and returns the answer of a call that worked.
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}/{path}", self.url))
            .bearer_auth(self.key.expose());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let unreachable = |error: reqwest::Error| {
            if error.is_timeout() {
                SplitwiseError::Timeout
            } else {
                SplitwiseError::Unavailable(error.to_string())
            }
        };
        let response = request.send().await.map_err(unreachable)?;
        let status = response.status();
        let text = response.text().await.map_err(unreachable)?;
        let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let errors = shorten(&errors(&answer).join("; "));
        if !status.is_success() {
            tracing::warn!(%status, path, errors, "Splitwise refused a request");
            return Err(match status {
                StatusCode::UNAUTHORIZED => SplitwiseError::Unauthorized,
                StatusCode::NOT_FOUND => SplitwiseError::NotFound("thing"),
                StatusCode::TOO_MANY_REQUESTS => {
                    SplitwiseError::Unavailable("too many requests: try again in a minute".into())
                }
                status if status.is_server_error() => {
                    SplitwiseError::Unavailable(format!("{status}"))
                }
                _ if errors.is_empty() => SplitwiseError::Refused(shorten(&text)),
                _ => SplitwiseError::Refused(errors),
            });
        }
        if !errors.is_empty() || answer["success"] == json!(false) {
            tracing::warn!(path, errors, "Splitwise turned a request down");
            return Err(SplitwiseError::Refused(if errors.is_empty() {
                "it didn't work".into()
            } else {
                errors
            }));
        }
        Ok(answer)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.call(Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.call(Method::POST, path, Some(body)).await
    }
}

/// The error messages of an answer: `error`, and every message in `errors`
/// (`{"base": [...], "cost": [...]}` or a list), with their field.
fn errors(answer: &Value) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(error) = answer["error"].as_str() {
        found.push(error.to_string());
    }
    let texts = |value: &Value| -> Vec<String> {
        match value {
            Value::String(text) => vec![text.clone()],
            Value::Array(values) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    };
    match &answer["errors"] {
        Value::Object(fields) => {
            for (field, value) in fields {
                for text in texts(value) {
                    found.push(if field == "base" {
                        text
                    } else {
                        format!("{field} {text}")
                    });
                }
            }
        }
        other => found.extend(texts(other)),
    }
    found
}

fn shorten(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_ERROR {
        text.to_string()
    } else {
        text.chars().take(MAX_ERROR).chain(['…']).collect()
    }
}

/// `value[key]` as a `T`.
fn field<T: for<'de> Deserialize<'de>>(value: &Value, key: &str) -> Result<T> {
    T::deserialize(&value[key]).map_err(|error| SplitwiseError::Invalid(format!("{key}: {error}")))
}

#[derive(Deserialize)]
struct RawUser {
    id: i64,
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
}

impl From<RawUser> for User {
    fn from(user: RawUser) -> Self {
        Self {
            id: user.id,
            first_name: user.first_name.unwrap_or_default(),
            last_name: user.last_name,
        }
    }
}

#[derive(Deserialize)]
struct RawGroup {
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    members: Vec<RawUser>,
}

impl From<RawGroup> for Group {
    fn from(group: RawGroup) -> Self {
        Self {
            id: group.id,
            name: group.name,
            members: group.members.into_iter().map(User::from).collect(),
        }
    }
}

#[derive(Deserialize)]
struct RawCategory {
    id: i64,
    name: String,
    #[serde(default)]
    subcategories: Vec<RawCategory>,
}

#[derive(Deserialize)]
struct RawExpense {
    id: i64,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
}

impl From<RawExpense> for Stored {
    fn from(expense: RawExpense) -> Self {
        Self {
            id: expense.id,
            updated_at: expense.updated_at.unwrap_or_default(),
            deleted: expense.deleted_at.is_some(),
        }
    }
}

/// An amount as Splitwise takes it: `12.50`.
fn amount(value: Decimal) -> String {
    format!("{:.2}", value)
}

/// The request creating or replacing `expense`.
fn expense_body(expense: &Expense) -> Value {
    let mut body = Map::new();
    body.insert("group_id".into(), json!(expense.group));
    body.insert("description".into(), json!(expense.description));
    body.insert("details".into(), json!(expense.details));
    body.insert("cost".into(), json!(amount(expense.cost)));
    body.insert("currency_code".into(), json!(expense.currency));
    // Noon UTC, so that the day is the same in every timezone around it.
    let date = format!("{}T12:00:00Z", expense.date.format("%Y-%m-%d"));
    body.insert("date".into(), json!(date));
    if let Some(category) = expense.category {
        body.insert("category_id".into(), json!(category));
    }
    if expense.payment {
        body.insert("payment".into(), json!(true));
    }
    for (index, share) in expense.shares.iter().enumerate() {
        let key = |field: &str| format!("users__{index}__{field}");
        body.insert(key("user_id"), json!(share.user));
        body.insert(key("paid_share"), json!(amount(share.paid)));
        body.insert(key("owed_share"), json!(amount(share.owed)));
    }
    Value::Object(body)
}

/// The one expense a create or an update answers with.
fn saved(answer: &Value) -> Result<Stored> {
    let expenses: Vec<RawExpense> = field(answer, "expenses")?;
    expenses
        .into_iter()
        .next()
        .map(Stored::from)
        .ok_or_else(|| SplitwiseError::Invalid("no expense in the answer".into()))
}

impl Splitwise for SplitwiseApi {
    fn groups(&self) -> BoxFuture<'_, Result<Vec<Group>>> {
        Box::pin(async move {
            let answer = self.get("get_groups").await?;
            let groups: Vec<RawGroup> = field(&answer, "groups")?;
            // Group 0 holds the expenses outside of any group.
            Ok(groups
                .into_iter()
                .filter(|group| group.id != 0)
                .map(Group::from)
                .collect())
        })
    }

    fn group(&self, id: i64) -> BoxFuture<'_, Result<Group>> {
        Box::pin(async move {
            let answer = match self.get(&format!("get_group/{id}")).await {
                Err(SplitwiseError::NotFound(_)) => return Err(SplitwiseError::NotFound("group")),
                other => other?,
            };
            Ok(field::<RawGroup>(&answer, "group")?.into())
        })
    }

    fn create_group<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Group>> {
        Box::pin(async move {
            let body = json!({"name": name, "group_type": "trip"});
            let answer = self.post("create_group", body).await?;
            Ok(field::<RawGroup>(&answer, "group")?.into())
        })
    }

    fn add_to_group<'a>(&'a self, group: i64, person: &'a Person) -> BoxFuture<'a, Result<User>> {
        Box::pin(async move {
            let body = match person {
                Person::User(user) => json!({"group_id": group, "user_id": user}),
                Person::Invite {
                    first_name,
                    last_name,
                    email,
                } => json!({
                    "group_id": group,
                    "first_name": first_name,
                    "last_name": last_name,
                    "email": email,
                }),
            };
            let answer = self.post("add_user_to_group", body).await?;
            Ok(field::<RawUser>(&answer, "user")?.into())
        })
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>>> {
        Box::pin(async move {
            let answer = self.get("get_categories").await?;
            let parents: Vec<RawCategory> = field(&answer, "categories")?;
            Ok(parents
                .into_iter()
                .flat_map(|parent| {
                    let name = parent.name;
                    parent
                        .subcategories
                        .into_iter()
                        .map(move |category| Category {
                            id: category.id,
                            parent: name.clone(),
                            name: category.name,
                        })
                })
                .collect())
        })
    }

    fn create_expense<'a>(&'a self, expense: &'a Expense) -> BoxFuture<'a, Result<Stored>> {
        Box::pin(async move { saved(&self.post("create_expense", expense_body(expense)).await?) })
    }

    fn update_expense<'a>(
        &'a self,
        id: i64,
        expense: &'a Expense,
    ) -> BoxFuture<'a, Result<Stored>> {
        Box::pin(async move {
            let path = format!("update_expense/{id}");
            saved(&self.post(&path, expense_body(expense)).await?)
        })
    }

    fn expense(&self, id: i64) -> BoxFuture<'_, Result<Option<Stored>>> {
        Box::pin(async move {
            match self.get(&format!("get_expense/{id}")).await {
                Ok(answer) => Ok(Some(field::<RawExpense>(&answer, "expense")?.into())),
                Err(SplitwiseError::NotFound(_)) => Ok(None),
                Err(error) => Err(error),
            }
        })
    }

    fn delete_expense(&self, id: i64) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            match self.post(&format!("delete_expense/{id}"), json!({})).await {
                Ok(_) | Err(SplitwiseError::NotFound(_)) => Ok(()),
                // "does not exist, or has already been deleted"
                Err(SplitwiseError::Refused(error)) if error.contains("does not exist") => Ok(()),
                Err(error) => Err(error),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use rust_decimal::dec;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;
    use crate::splitwise::Share;

    /// A one-request HTTP server answering `status` and `body`; the handle
    /// gives back the request it got.
    async fn serve(status: u16, body: Value) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/v3.0", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buffer = [0; 4096];
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
                 {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(received).unwrap()
        });
        (url, handle)
    }

    fn api(url: &str) -> SplitwiseApi {
        SplitwiseApi::new(url, Secret::new("sw-key".into()), Duration::from_secs(10)).unwrap()
    }

    fn dinner() -> Expense {
        Expense {
            group: 7,
            description: "Dinner".into(),
            details: "Curry: 1,200".into(),
            cost: dec!(1500),
            currency: "JPY".into(),
            date: NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
            category: Some(13),
            payment: false,
            shares: vec![
                Share {
                    user: 11,
                    paid: dec!(1500),
                    owed: dec!(500.5),
                },
                Share {
                    user: 12,
                    paid: dec!(0),
                    owed: dec!(999.5),
                },
            ],
        }
    }

    #[tokio::test]
    async fn expenses_are_sent_with_their_shares_flattened() {
        let answer = json!({
            "expenses": [{"id": 900, "updated_at": "2026-10-13T10:00:00Z", "deleted_at": null}],
            "errors": {},
        });
        let (url, server) = serve(200, answer).await;
        let stored = api(&url).create_expense(&dinner()).await.unwrap();
        assert_eq!(
            stored,
            Stored {
                id: 900,
                updated_at: "2026-10-13T10:00:00Z".into(),
                deleted: false,
            }
        );

        let received = server.await.unwrap();
        let (head, body) = received.split_once("\r\n\r\n").unwrap();
        let head = head.to_lowercase();
        assert!(head.starts_with("post /api/v3.0/create_expense "), "{head}");
        assert!(head.contains("authorization: bearer sw-key"), "{head}");
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            body,
            json!({
                "group_id": 7,
                "description": "Dinner",
                "details": "Curry: 1,200",
                "cost": "1500.00",
                "currency_code": "JPY",
                "date": "2026-10-04T12:00:00Z",
                "category_id": 13,
                "users__0__user_id": 11,
                "users__0__paid_share": "1500.00",
                "users__0__owed_share": "500.50",
                "users__1__user_id": 12,
                "users__1__paid_share": "0.00",
                "users__1__owed_share": "999.50",
            })
        );
    }

    #[tokio::test]
    async fn payments_say_so() {
        let answer = json!({"expenses": [{"id": 901, "updated_at": "x"}], "errors": {}});
        let (url, server) = serve(200, answer).await;
        let payment = Expense {
            payment: true,
            category: None,
            ..dinner()
        };
        api(&url).update_expense(901, &payment).await.unwrap();
        let received = server.await.unwrap();
        assert!(
            received
                .to_lowercase()
                .starts_with("post /api/v3.0/update_expense/901 ")
        );
        let body: Value = serde_json::from_str(received.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["payment"], true);
        assert!(body.get("category_id").is_none());
    }

    #[tokio::test]
    async fn errors_in_an_ok_answer_are_errors() {
        let answer = json!({
            "expenses": [],
            "errors": {"base": ["You have reached your daily limit"], "cost": ["must be positive"]},
        });
        let (url, _server) = serve(200, answer).await;
        let error = api(&url).create_expense(&dinner()).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "Splitwise said: You have reached your daily limit; cost must be positive"
        );

        let (url, _server) = serve(200, json!({"success": false, "user": null})).await;
        let error = api(&url)
            .add_to_group(7, &Person::User(3))
            .await
            .unwrap_err();
        assert!(matches!(error, SplitwiseError::Refused(_)), "{error:?}");

        let (url, _server) = serve(401, json!({"error": "Invalid API request"})).await;
        let error = api(&url).groups().await.unwrap_err();
        assert!(matches!(error, SplitwiseError::Unauthorized), "{error:?}");
    }

    #[tokio::test]
    async fn missing_and_deleted_expenses() {
        let (url, _server) = serve(404, json!({"errors": {"base": ["Not found"]}})).await;
        assert_eq!(api(&url).expense(5).await.unwrap(), None);

        let deleted = json!({"expense": {"id": 5, "updated_at": "y", "deleted_at": "z"}});
        let (url, _server) = serve(200, deleted).await;
        assert!(api(&url).expense(5).await.unwrap().unwrap().deleted);

        let gone = json!({
            "success": false,
            "errors": {"expense": ["does not exist, or has already been deleted"]},
        });
        let (url, _server) = serve(200, gone).await;
        api(&url).delete_expense(5).await.unwrap();
    }

    #[tokio::test]
    async fn groups_and_categories_are_read() {
        let groups = json!({"groups": [
            {"id": 0, "name": "Non-group expenses", "members": []},
            {"id": 7, "name": "Japan", "members": [
                {"id": 11, "first_name": "Ann", "last_name": "Lee", "email": "ann@example.com"},
                {"id": 12, "first_name": "Bob", "last_name": null},
            ]},
        ]});
        let (url, _server) = serve(200, groups).await;
        let groups = api(&url).groups().await.unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Japan");
        let names: Vec<String> = groups[0].members.iter().map(User::name).collect();
        assert_eq!(names, ["Ann Lee", "Bob"]);

        let categories = json!({"categories": [
            {"id": 25, "name": "Food and drink", "subcategories": [
                {"id": 13, "name": "Dining out"},
                {"id": 12, "name": "Groceries"},
            ]},
        ]});
        let (url, _server) = serve(200, categories).await;
        let categories = api(&url).categories().await.unwrap();
        assert_eq!(
            categories[0],
            Category {
                id: 13,
                parent: "Food and drink".into(),
                name: "Dining out".into(),
            }
        );
    }

    /// Run with a real key in `ASSISTANT_SPLITWISE__API_KEY`: reads the
    /// account's groups, and changes nothing.
    #[tokio::test]
    #[ignore = "calls Splitwise"]
    async fn real_api_lists_groups() {
        let key = std::env::var("ASSISTANT_SPLITWISE__API_KEY").unwrap();
        let api =
            SplitwiseApi::new(DEFAULT_URL, Secret::new(key), Duration::from_secs(30)).unwrap();
        let groups = api.groups().await.unwrap();
        println!("{} groups", groups.len());
        assert!(!api.categories().await.unwrap().is_empty());
    }
}
