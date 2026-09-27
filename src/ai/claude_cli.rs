//! The Claude Code CLI as a model: `claude -p` with a JSON schema and no
//! tools, run in an empty directory with only the subscription token.
//!
//! Without tools, a message crafted to trick the model can't read files or
//! run commands: the worst it can do is fill the schema wrongly, which the
//! features check.

use std::{io::ErrorKind, path::Path, process::Stdio, time::Duration};

use futures::future::BoxFuture;
use serde::Deserialize;
use tokio::{io::AsyncWriteExt, process::Command, sync::Semaphore};

use super::{AiError, Llm, Request};
use crate::config::{AiConfig, Secret};

/// The longest error text kept from the CLI.
const MAX_ERROR: usize = 300;

pub struct ClaudeCli {
    program: String,
    token: Secret<String>,
    timeout: Duration,
    permits: Semaphore,
}

impl std::fmt::Debug for ClaudeCli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeCli")
            .field("program", &self.program)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl ClaudeCli {
    /// The CLI configured by `[ai]`, if a token is.
    pub fn from_config(config: &AiConfig) -> Option<Self> {
        if !config.is_configured() {
            return None;
        }
        Some(Self::new(
            config.claude_path.as_deref().unwrap_or("claude"),
            config.oauth_token.clone()?,
            Duration::from_secs(config.timeout_secs),
            config.max_concurrent,
        ))
    }

    pub fn new(
        program: &str,
        token: Secret<String>,
        timeout: Duration,
        max_concurrent: usize,
    ) -> Self {
        Self {
            program: program.to_string(),
            token,
            timeout,
            permits: Semaphore::new(max_concurrent.max(1)),
        }
    }

    fn command(&self, request: &Request, schema: &str, dir: &Path) -> Command {
        let mut command = Command::new(&self.program);
        // Not `--bare`, which ignores subscription tokens: an empty HOME and
        // working directory leave no settings, hooks, plugins or CLAUDE.md to
        // load, and the flags turn off tools, MCP servers and skills.
        command
            .args(["-p", "--tools", "", "--strict-mcp-config"])
            .args(["--disable-slash-commands", "--no-session-persistence"])
            .args(["--output-format", "json", "--json-schema", schema])
            .args([
                "--system-prompt",
                &request.system,
                "--model",
                &request.model,
            ])
            .current_dir(dir)
            // Nothing of the bot's environment, nor the owner's Claude setup.
            .env_clear()
            .env("HOME", dir)
            .env("CLAUDE_CODE_OAUTH_TOKEN", self.token.expose())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        command
    }

    async fn run(&self, request: &Request) -> Result<serde_json::Value, AiError> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| AiError::Unavailable("shutting down".into()))?;
        let dir = tempfile::tempdir().map_err(|error| AiError::Unavailable(error.to_string()))?;
        let schema = request.schema.to_string();

        let mut child = self
            .command(request, &schema, dir.path())
            .spawn()
            .map_err(|error| match error.kind() {
                ErrorKind::NotFound => AiError::Unavailable(format!(
                    "`{}` was not found: install Claude Code, or set ai.claude_path",
                    self.program
                )),
                _ => AiError::Unavailable(error.to_string()),
            })?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let exchange = async {
            // The CLI may exit without reading it (e.g. not logged in): its
            // output then says why.
            if let Err(error) = stdin.write_all(request.text.as_bytes()).await {
                tracing::debug!(%error, "the Claude CLI didn't take the message");
            }
            drop(stdin);
            child.wait_with_output().await
        };
        let output = tokio::time::timeout(self.timeout, exchange)
            .await
            .map_err(|_| AiError::Timeout)?
            .map_err(|error| AiError::Unavailable(error.to_string()))?;

        match serde_json::from_slice::<Output>(&output.stdout) {
            Ok(result) => result.into_answer(),
            Err(_) if !output.status.success() => Err(AiError::Failed(shorten(
                &String::from_utf8_lossy(&output.stderr),
            ))),
            Err(error) => Err(AiError::Invalid(format!("unreadable output: {error}"))),
        }
    }
}

/// What `claude -p --output-format json` prints.
#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    structured_output: Option<serde_json::Value>,
}

impl Output {
    fn into_answer(self) -> Result<serde_json::Value, AiError> {
        if self.is_error {
            let reason = self.result.or(self.subtype).unwrap_or_default();
            return Err(AiError::Failed(shorten(&reason)));
        }
        if let Some(answer) = self.structured_output {
            return Ok(answer);
        }
        // Older CLIs put the JSON in the result text.
        let text = self.result.unwrap_or_default();
        let json = text
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```");
        serde_json::from_str(json.trim())
            .map_err(|_| AiError::Invalid(format!("no structured answer in {}", shorten(&text))))
    }
}

fn shorten(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_ERROR {
        text.to_string()
    } else {
        text.chars().take(MAX_ERROR).chain(['…']).collect()
    }
}

impl Llm for ClaudeCli {
    fn complete<'a>(
        &'a self,
        request: &'a Request,
    ) -> BoxFuture<'a, Result<serde_json::Value, AiError>> {
        Box::pin(self.run(request))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};

    use serde_json::json;

    use super::*;

    /// Scripts are written then run: one at a time, so that no other test
    /// forks while a script is open for writing (which makes running it fail).
    static SCRIPTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn request() -> Request {
        Request {
            system: "Extract the answer.".into(),
            text: "the answer is 42".into(),
            schema: json!({"type": "object"}),
            model: "haiku".into(),
        }
    }

    /// A fake `claude` running `body`, which may use `$LOG` (a directory
    /// kept after the run).
    fn script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("claude");
        let log = dir.join("log");
        std::fs::create_dir_all(&log).unwrap();
        std::fs::write(
            &path,
            format!("#!/bin/sh\nLOG='{}'\n{body}\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn cli(program: &Path, timeout: Duration) -> ClaudeCli {
        ClaudeCli::new(
            program.to_str().unwrap(),
            Secret::new("sk-test".into()),
            timeout,
            1,
        )
    }

    #[tokio::test]
    async fn the_cli_gets_the_request_and_nothing_else() {
        let _serial = SCRIPTS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let program = script(
            dir.path(),
            r#"printf '%s\n' "$@" > "$LOG/args"
cat > "$LOG/stdin"
env > "$LOG/env"
echo '{"type":"result","is_error":false,"result":"","structured_output":{"answer":42}}'"#,
        );
        let answer = cli(&program, Duration::from_secs(10))
            .complete(&request())
            .await
            .unwrap();
        assert_eq!(answer, json!({"answer": 42}));

        let log = dir.path().join("log");
        let args = std::fs::read_to_string(log.join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(
            args,
            [
                "-p",
                "--tools",
                "",
                "--strict-mcp-config",
                "--disable-slash-commands",
                "--no-session-persistence",
                "--output-format",
                "json",
                "--json-schema",
                r#"{"type":"object"}"#,
                "--system-prompt",
                "Extract the answer.",
                "--model",
                "haiku"
            ]
        );
        assert_eq!(
            std::fs::read_to_string(log.join("stdin")).unwrap(),
            "the answer is 42"
        );
        let env = std::fs::read_to_string(log.join("env")).unwrap();
        assert!(env.contains("CLAUDE_CODE_OAUTH_TOKEN=sk-test"), "{env}");
        // Cargo sets these for tests: they must not leak.
        assert!(!env.contains("CARGO_PKG_NAME"), "{env}");
    }

    #[tokio::test]
    async fn errors_timeouts_and_old_outputs() {
        let _serial = SCRIPTS.lock().await;
        let dir = tempfile::tempdir().unwrap();

        let failing = script(
            dir.path(),
            r#"echo '{"type":"result","is_error":true,"result":"Invalid API key"}'; exit 1"#,
        );
        let error = cli(&failing, Duration::from_secs(10))
            .complete(&request())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, AiError::Failed(reason) if reason == "Invalid API key"),
            "{error}"
        );

        let crashing = script(dir.path(), "echo 'boom' >&2; exit 2");
        let error = cli(&crashing, Duration::from_secs(10))
            .complete(&request())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, AiError::Failed(reason) if reason == "boom"),
            "{error}"
        );

        let old = script(
            dir.path(),
            r#"printf '%s\n' '{"type":"result","is_error":false,"result":"```json\n{\"answer\": 1}\n```"}'"#,
        );
        let answer = cli(&old, Duration::from_secs(10))
            .complete(&request())
            .await
            .unwrap();
        assert_eq!(answer, json!({"answer": 1}));

        let slow = script(dir.path(), "exec sleep 5");
        let error = cli(&slow, Duration::from_millis(200))
            .complete(&request())
            .await
            .unwrap_err();
        assert!(matches!(error, AiError::Timeout), "{error}");

        let missing = dir.path().join("nothing-here");
        let error = cli(&missing, Duration::from_secs(1))
            .complete(&request())
            .await
            .unwrap_err();
        assert!(matches!(error, AiError::Unavailable(_)), "{error}");
    }

    /// Asks the real Claude, with `ASSISTANT_AI__OAUTH_TOKEN` set:
    /// `cargo test -- --ignored real_claude`.
    #[tokio::test]
    #[ignore = "uses the Claude subscription"]
    async fn real_claude_fills_a_schema() {
        let token = std::env::var("ASSISTANT_AI__OAUTH_TOKEN").expect("a token");
        let cli = ClaudeCli::new("claude", Secret::new(token), Duration::from_secs(90), 1);
        let answer = cli
            .complete(&Request {
                schema: json!({
                    "type": "object",
                    "properties": {"number": {"type": "string"}},
                    "required": ["number"],
                    "additionalProperties": false,
                }),
                system: "Copy the number written in the message, exactly as written.".into(),
                ..request()
            })
            .await
            .unwrap();
        assert_eq!(answer, json!({"number": "42"}));
    }
}
