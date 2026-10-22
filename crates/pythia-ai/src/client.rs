//! Sending a conversation to a backend and reading the answer.
//!
//! Calls block until the answer is complete (no streaming); run them on a
//! worker thread. Local endpoints (localhost, 127.0.0.1, ::1) are reached
//! directly, others through the proxy from `HTTPS_PROXY` / `ALL_PROXY` and
//! `NO_PROXY`.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pythia_config::{AiBackend, AiKind};
use serde_json::{Value, json};

/// The Messages API version the requests are written for.
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Beta header for `fallbacks: "default"`: a declined request is answered
/// by the model Anthropic recommends for the refusal's category.
const SERVER_FALLBACK: &str = "server-side-fallback-2026-07-01";
/// `max_tokens` when a backend has none (the Messages API requires one).
const DEFAULT_MAX_TOKENS: u32 = 16000;
/// Answers larger than this are refused.
const MAX_RESPONSE: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatRequest {
    pub system: String,
    /// Alternating user and assistant messages, ending with the user's.
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The backend is missing something (key, endpoint, command).
    #[error("{0}")]
    Setup(String),
    /// The server answered with an error status.
    #[error("{message} (HTTP {status})")]
    Status { status: u16, message: String },
    /// The server couldn't be reached or the connection broke.
    #[error("{0}")]
    Transport(String),
    /// The answer wasn't what the API documents, or had no text.
    #[error("{0}")]
    Answer(String),
    /// The command line tool failed.
    #[error("{0}")]
    Command(String),
    #[error("no answer within {0} s")]
    Timeout(u64),
    #[error("cancelled")]
    Cancelled,
}

/// Sends `request` to `backend` and returns the answer's text. `key` is the
/// backend's API key, if it uses one. `cancel` stops waiting for a command
/// line tool (and kills it); HTTP requests run until they end or time out.
pub fn complete(
    backend: &AiBackend,
    key: Option<&str>,
    request: &ChatRequest,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<String, Error> {
    match backend.kind {
        AiKind::Anthropic => {
            let key = key
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| missing_key(backend))?;
            let mut headers = vec![
                ("x-api-key", key.trim().to_owned()),
                ("anthropic-version", ANTHROPIC_VERSION.to_owned()),
            ];
            if server_fallback(backend) {
                headers.push(("anthropic-beta", SERVER_FALLBACK.to_owned()));
            }
            let body = anthropic_body(backend, request);
            let (status, text) = post(endpoint(backend)?, &headers, &body, timeout)?;
            parse_anthropic(status, &text)
        }
        AiKind::OpenAi | AiKind::LocalServer => {
            let key = key.map(str::trim).filter(|k| !k.is_empty());
            if backend.kind == AiKind::OpenAi && key.is_none() {
                return Err(missing_key(backend));
            }
            let headers: Vec<(&str, String)> = key
                .map(|k| ("authorization", format!("Bearer {k}")))
                .into_iter()
                .collect();
            let body = openai_body(backend, request);
            let (status, text) = post(endpoint(backend)?, &headers, &body, timeout)?;
            parse_openai(status, &text)
        }
        AiKind::Cli => {
            if backend.command.trim().is_empty() {
                return Err(Error::Setup(format!(
                    "{} has no command line (Settings → AI)",
                    backend.name
                )));
            }
            run_command(&backend.command, &cli_prompt(request), timeout, cancel)
        }
    }
}

fn missing_key(backend: &AiBackend) -> Error {
    Error::Setup(format!(
        "{} has no API key; enter it in Settings → AI",
        backend.name
    ))
}

fn endpoint(backend: &AiBackend) -> Result<&str, Error> {
    let url = backend.endpoint.trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(url)
    } else {
        Err(Error::Setup(format!(
            "{} needs an http:// or https:// endpoint (Settings → AI)",
            backend.name
        )))
    }
}

/// Request body for the Anthropic Messages API.
pub fn anthropic_body(backend: &AiBackend, request: &ChatRequest) -> Value {
    let max_tokens = match backend.max_tokens {
        0 => DEFAULT_MAX_TOKENS,
        n => n,
    };
    let mut body = json!({
        "model": backend.model.trim(),
        "max_tokens": max_tokens,
        "messages": messages(request),
    });
    if !request.system.is_empty() {
        body["system"] = json!(request.system);
    }
    if server_fallback(backend) {
        body["fallbacks"] = json!("default");
    }
    body
}

/// Whether to ask for a server-side fallback when the model declines: only
/// the Claude API itself has them (not gateways or other platforms), and
/// only for the models listed (Claude Haiku 5.5 has none).
fn server_fallback(backend: &AiBackend) -> bool {
    let on_claude_api = backend
        .endpoint
        .trim()
        .starts_with("https://api.anthropic.com/");
    let model = backend.model.trim();
    on_claude_api
        && [
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-fable-5-1",
            "claude-fable-5",
            "claude-sonnet-5-5",
        ]
        .contains(&model)
}

/// Request body for OpenAI-compatible chat completions: the system prompt
/// is the first message.
pub fn openai_body(backend: &AiBackend, request: &ChatRequest) -> Value {
    let mut all = Vec::new();
    if !request.system.is_empty() {
        all.push(json!({"role": "system", "content": request.system}));
    }
    if let Value::Array(rest) = messages(request) {
        all.extend(rest);
    }
    let mut body = json!({"messages": all, "stream": false});
    if !backend.model.trim().is_empty() {
        body["model"] = json!(backend.model.trim());
    }
    if backend.max_tokens > 0 {
        body["max_tokens"] = json!(backend.max_tokens);
    }
    body
}

fn messages(request: &ChatRequest) -> Value {
    Value::Array(
        request
            .messages
            .iter()
            .map(|m| json!({"role": m.role.name(), "content": m.content}))
            .collect(),
    )
}

/// The answer of a Messages API response: its text blocks (thinking and
/// other blocks are skipped).
pub fn parse_anthropic(status: u16, body: &str) -> Result<String, Error> {
    let value = parse_json(status, body)?;
    let stop = value["stop_reason"].as_str().unwrap_or_default();
    if stop == "refusal" {
        return Err(Error::Answer(
            "The model declined to answer this request".into(),
        ));
    }
    let blocks = value["content"]
        .as_array()
        .ok_or_else(|| Error::Answer(format!("no content in the answer: {}", excerpt(body))))?;
    let text: Vec<&str> = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    finish(text.concat(), stop == "max_tokens")
}

/// The answer of a chat completions response.
pub fn parse_openai(status: u16, body: &str) -> Result<String, Error> {
    let value = parse_json(status, body)?;
    let choice = &value["choices"][0];
    let message = &choice["message"];
    if let Some(refusal) = message["refusal"].as_str().filter(|r| !r.is_empty()) {
        return Err(Error::Answer(format!("The model declined: {refusal}")));
    }
    let Some(text) = message["content"].as_str() else {
        return Err(Error::Answer(format!(
            "no message in the answer: {}",
            excerpt(body)
        )));
    };
    finish(text.to_owned(), choice["finish_reason"] == "length")
}

fn finish(text: String, cut_off: bool) -> Result<String, Error> {
    match (text.trim().is_empty(), cut_off) {
        (true, true) => Err(Error::Answer(
            "The answer hit the token limit before any text (raise Max answer tokens)".into(),
        )),
        (true, false) => Err(Error::Answer("The answer was empty".into())),
        (false, true) => Ok(format!(
            "{text}\n\n(The answer was cut off at the token limit.)"
        )),
        (false, false) => Ok(text),
    }
}

/// The JSON of a successful response, or the error the server reported:
/// `{"error": {"message": …}}` (Anthropic, OpenAI, llama.cpp) or
/// `{"error": "…"}` (Ollama).
fn parse_json(status: u16, body: &str) -> Result<Value, Error> {
    let value: Result<Value, _> = serde_json::from_str(body);
    if !(200..300).contains(&status) {
        let message = value
            .ok()
            .and_then(|v| {
                let error = &v["error"];
                error["message"]
                    .as_str()
                    .or(error.as_str())
                    .or(v["message"].as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| excerpt(body));
        return Err(Error::Status { status, message });
    }
    value.map_err(|e| Error::Answer(format!("the answer isn't JSON ({e}): {}", excerpt(body))))
}

fn excerpt(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(300) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_owned(),
    }
}

/// POSTs JSON and returns the status and body, whatever the status.
fn post(
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
    timeout: Duration,
) -> Result<(u16, String), Error> {
    let proxy = if is_local(url) {
        None
    } else {
        ureq::Proxy::try_from_env()
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .proxy(proxy)
        .build()
        .into();
    let mut request = agent
        .post(url)
        .header("content-type", "application/json")
        .header("user-agent", concat!("Pythia/", env!("CARGO_PKG_VERSION")));
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let failed = |e: ureq::Error| match e {
        ureq::Error::Timeout(_) => Error::Timeout(timeout.as_secs()),
        ureq::Error::HostNotFound => Error::Transport(format!("host not found: {url}")),
        ureq::Error::ConnectionFailed => Error::Transport(format!("could not connect to {url}")),
        ureq::Error::Io(e) => Error::Transport(format!("{url}: {e}")),
        e => Error::Transport(e.to_string()),
    };
    let mut response = request.send(body.to_string()).map_err(failed)?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE)
        .read_to_string()
        .map_err(failed)?;
    Ok((status, text))
}

/// Whether the URL's host is this machine.
fn is_local(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    host.eq_ignore_ascii_case("localhost") || host == "::1" || host.starts_with("127.")
}

/// The conversation as one text for a command line tool.
pub fn cli_prompt(request: &ChatRequest) -> String {
    let mut out = String::new();
    if !request.system.is_empty() {
        out.push_str(&request.system);
        out.push_str("\n\n");
    }
    let earlier = request.messages.len().saturating_sub(1);
    for m in &request.messages[..earlier] {
        let who = match m.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        out.push_str(&format!("{who}:\n{}\n\n", m.content));
    }
    if let Some(last) = request.messages.last() {
        if earlier > 0 {
            out.push_str("User:\n");
        }
        out.push_str(&last.content);
        out.push('\n');
    }
    out
}

/// Runs `command` with the system's shell, writes `input` to its stdin and
/// returns its stdout. It is killed when `cancel` is set or after `timeout`.
pub fn run_command(
    command: &str,
    input: &str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<String, Error> {
    let mut child = shell(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Command(format!("could not start {command}: {e}")))?;
    let stdin = child.stdin.take();
    let input = input.to_owned();
    // Feed and drain on threads: a tool may answer before reading all input.
    std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(input.as_bytes());
        }
    });
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(Error::Command(format!("{command}: {e}"))),
        }
        let stop = if cancel.load(Ordering::Relaxed) {
            Some(Error::Cancelled)
        } else if started.elapsed() >= timeout {
            Some(Error::Timeout(timeout.as_secs()))
        } else {
            None
        };
        if let Some(error) = stop {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    let err = err.trim();
    if !status.success() {
        let detail = if err.is_empty() {
            String::new()
        } else {
            format!(": {}", excerpt(err))
        };
        return Err(Error::Command(format!(
            "{command} failed ({status}){detail}"
        )));
    }
    if out.trim().is_empty() {
        return Err(Error::Command(if err.is_empty() {
            format!("{command} printed no answer")
        } else {
            format!("{command} printed no answer: {}", excerpt(err))
        }));
    }
    Ok(out)
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// `sh -c` or `cmd /C`, so the command line can use the shell's search
/// path, quoting and wrappers (`.cmd` scripts on Windows).
fn shell(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut c = Command::new("cmd");
        c.arg("/C")
            .raw_arg(command)
            .creation_flags(CREATE_NO_WINDOW);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    use super::*;

    fn backend(kind: AiKind, url: &str) -> AiBackend {
        AiBackend {
            endpoint: url.to_owned(),
            model: "test-model".into(),
            ..AiBackend::new(kind, "t".into())
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            system: "Be brief.".into(),
            messages: vec![
                Message {
                    role: Role::User,
                    content: "Explain dual".into(),
                },
                Message {
                    role: Role::Assistant,
                    content: "A one-row table.".into(),
                },
                Message {
                    role: Role::User,
                    content: "Why one row?".into(),
                },
            ],
        }
    }

    /// The raw request a [`serve`]d endpoint got: head (lowercase) and body.
    struct Received {
        head: String,
        body: Value,
    }

    /// Answers one HTTP request on a local port with `status` and `answer`.
    fn serve(status: u16, answer: &'static str) -> (String, JoinHandle<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/endpoint", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            let head_end = loop {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "connection closed before the request was complete");
                data.extend_from_slice(&buf[..n]);
                if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&data[..head_end]).to_lowercase();
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse().unwrap());
            while data.len() < head_end + length {
                let n = stream.read(&mut buf).unwrap();
                data.extend_from_slice(&buf[..n]);
            }
            let body = serde_json::from_slice(&data[head_end..head_end + length]).unwrap();
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                answer.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            Received { head, body }
        });
        (url, handle)
    }

    fn call(backend: &AiBackend, key: Option<&str>) -> Result<String, Error> {
        complete(
            backend,
            key,
            &request(),
            Duration::from_secs(10),
            &AtomicBool::new(false),
        )
    }

    #[test]
    fn server_fallback_only_on_the_claude_api() {
        let request = ChatRequest {
            system: String::new(),
            messages: Vec::new(),
        };
        let mut b = AiBackend::new(AiKind::Anthropic, "a".into());
        assert_eq!(anthropic_body(&b, &request)["fallbacks"], "default");
        b.model = "claude-haiku-5-5".into();
        assert!(anthropic_body(&b, &request).get("fallbacks").is_none());
        b.model = "claude-opus-5-5".into();
        b.endpoint = "https://gateway.example.com/v1/messages".into();
        assert!(anthropic_body(&b, &request).get("fallbacks").is_none());
    }

    #[test]
    fn anthropic_request_and_answer() {
        let (url, server) = serve(
            200,
            r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"thinking","thinking":""},{"type":"text","text":"Because "},{"type":"text","text":"it has one row."}],"stop_reason":"end_turn"}"#,
        );
        let answer = call(&backend(AiKind::Anthropic, &url), Some("sk-test\n")).unwrap();
        assert_eq!(answer, "Because it has one row.");
        let got = server.join().unwrap();
        assert!(got.head.starts_with("post /v1/endpoint http/1.1"));
        assert!(got.head.contains("x-api-key: sk-test\r\n"));
        assert!(got.head.contains("anthropic-version: 2026-10-21\r\n"));
        assert!(got.head.contains("content-type: application/json"));
        assert!(!got.head.contains("authorization"));
        assert_eq!(
            got.body,
            json!({
                "model": "test-model",
                "max_tokens": 16000,
                "system": "Be brief.",
                "messages": [
                    {"role": "user", "content": "Explain dual"},
                    {"role": "assistant", "content": "A one-row table."},
                    {"role": "user", "content": "Why one row?"},
                ],
            })
        );
    }

    #[test]
    fn anthropic_errors() {
        let (url, server) = serve(
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"},"request_id":"req_1"}"#,
        );
        let error = call(&backend(AiKind::Anthropic, &url), Some("bad")).unwrap_err();
        assert_eq!(
            error,
            Error::Status {
                status: 401,
                message: "invalid x-api-key".into()
            }
        );
        assert_eq!(error.to_string(), "invalid x-api-key (HTTP 401)");
        server.join().unwrap();

        assert!(matches!(
            parse_anthropic(200, r#"{"content":[],"stop_reason":"refusal"}"#),
            Err(Error::Answer(_))
        ));
        assert_eq!(
            parse_anthropic(
                200,
                r#"{"content":[{"type":"text","text":"select"}],"stop_reason":"max_tokens"}"#
            )
            .unwrap(),
            "select\n\n(The answer was cut off at the token limit.)"
        );
        assert!(matches!(
            parse_anthropic(529, "<html>overloaded</html>"),
            Err(Error::Status { status: 529, message }) if message == "<html>overloaded</html>"
        ));
        assert!(matches!(
            parse_anthropic(200, "not json"),
            Err(Error::Answer(_))
        ));
    }

    #[test]
    fn openai_request_and_answer() {
        let (url, server) = serve(
            200,
            r#"{"id":"c1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"One row by design."},"finish_reason":"stop"}]}"#,
        );
        let mut b = backend(AiKind::OpenAi, &url);
        b.max_tokens = 500;
        assert_eq!(call(&b, Some("key-1")).unwrap(), "One row by design.");
        let got = server.join().unwrap();
        assert!(got.head.contains("authorization: bearer key-1\r\n"));
        assert!(!got.head.contains("x-api-key"));
        assert_eq!(
            got.body,
            json!({
                "model": "test-model",
                "max_tokens": 500,
                "stream": false,
                "messages": [
                    {"role": "system", "content": "Be brief."},
                    {"role": "user", "content": "Explain dual"},
                    {"role": "assistant", "content": "A one-row table."},
                    {"role": "user", "content": "Why one row?"},
                ],
            })
        );
    }

    #[test]
    fn local_server_needs_no_key() {
        let (url, server) = serve(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"length"}]}"#,
        );
        let mut b = backend(AiKind::LocalServer, &url);
        b.model.clear();
        assert_eq!(
            call(&b, None).unwrap(),
            "ok\n\n(The answer was cut off at the token limit.)"
        );
        let got = server.join().unwrap();
        assert!(!got.head.contains("authorization"));
        assert!(got.body.get("model").is_none());
        assert!(got.body.get("max_tokens").is_none());
    }

    #[test]
    fn openai_error_shapes() {
        let (url, server) = serve(404, r#"{"error":"model 'llama9' not found"}"#);
        let error = call(&backend(AiKind::LocalServer, &url), None).unwrap_err();
        assert_eq!(error.to_string(), "model 'llama9' not found (HTTP 404)");
        server.join().unwrap();
        assert!(matches!(
            parse_openai(429, r#"{"error":{"message":"Rate limit reached","type":"requests"}}"#),
            Err(Error::Status { status: 429, message }) if message == "Rate limit reached"
        ));
        assert!(matches!(
            parse_openai(
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":null,"refusal":"no"}}]}"#
            ),
            Err(Error::Answer(_))
        ));
        assert!(matches!(
            parse_openai(200, r#"{"choices":[]}"#),
            Err(Error::Answer(_))
        ));
    }

    #[test]
    fn setup_problems_are_reported_before_sending() {
        let b = backend(AiKind::Anthropic, "https://api.anthropic.com/v1/messages");
        assert!(matches!(call(&b, None), Err(Error::Setup(_))));
        assert!(matches!(call(&b, Some("  ")), Err(Error::Setup(_))));
        let b = backend(AiKind::OpenAi, "https://api.openai.com/v1/chat/completions");
        assert!(matches!(call(&b, None), Err(Error::Setup(_))));
        let b = backend(AiKind::LocalServer, "localhost:11434/v1/chat/completions");
        assert!(matches!(call(&b, None), Err(Error::Setup(_))));
        let b = backend(AiKind::Cli, "");
        assert!(matches!(call(&b, None), Err(Error::Setup(_))));
    }

    #[test]
    fn unreachable_server() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
        let error = call(&backend(AiKind::LocalServer, &url), None).unwrap_err();
        assert!(matches!(error, Error::Transport(_)), "{error:?}");
    }

    #[test]
    fn local_hosts() {
        assert!(is_local("http://localhost:11434/v1/chat/completions"));
        assert!(is_local("http://127.0.0.1:8080/v1"));
        assert!(is_local("http://[::1]:8080/v1"));
        assert!(!is_local("https://api.anthropic.com/v1/messages"));
        assert!(!is_local("http://gpu-box.lan:11434/v1"));
    }

    #[test]
    fn conversation_as_one_prompt() {
        assert_eq!(
            cli_prompt(&request()),
            "Be brief.\n\nUser:\nExplain dual\n\nAssistant:\nA one-row table.\n\nUser:\nWhy one row?\n"
        );
        let single = ChatRequest {
            system: String::new(),
            messages: vec![Message {
                role: Role::User,
                content: "Hi".into(),
            }],
        };
        assert_eq!(cli_prompt(&single), "Hi\n");
    }

    /// Echoes stdin to stdout.
    #[cfg(windows)]
    const ECHO: &str = "more";
    #[cfg(not(windows))]
    const ECHO: &str = "cat";

    #[test]
    fn command_line_tool() {
        let mut b = backend(AiKind::Cli, "");
        b.command = ECHO.into();
        let answer = call(&b, None).unwrap();
        for line in [
            "Be brief.",
            "Explain dual",
            "A one-row table.",
            "Why one row?",
        ] {
            assert!(answer.contains(line), "{answer}");
        }

        b.command = "exit 3".into();
        let error = call(&b, None).unwrap_err();
        assert!(
            matches!(&error, Error::Command(m) if m.contains("exit 3 failed")),
            "{error:?}"
        );

        b.command = "pythia-no-such-tool-4711".into();
        assert!(matches!(call(&b, None), Err(Error::Command(_))));
    }

    #[cfg(unix)]
    #[test]
    fn command_line_tool_errors_timeout_and_cancel() {
        let never = AtomicBool::new(false);
        let error = run_command(
            "echo 'model not found' >&2; exit 1",
            "",
            Duration::from_secs(10),
            &never,
        )
        .unwrap_err();
        assert!(error.to_string().ends_with(": model not found"), "{error}");

        let started = Instant::now();
        let error = run_command("sleep 10", "", Duration::from_millis(200), &never).unwrap_err();
        assert_eq!(error, Error::Timeout(0));
        assert!(started.elapsed() < Duration::from_secs(5));

        let cancelled = AtomicBool::new(true);
        let error = run_command("sleep 10", "", Duration::from_secs(10), &cancelled).unwrap_err();
        assert_eq!(error, Error::Cancelled);

        let error = run_command("true", "", Duration::from_secs(10), &never).unwrap_err();
        assert!(error.to_string().contains("printed no answer"));
    }
}
