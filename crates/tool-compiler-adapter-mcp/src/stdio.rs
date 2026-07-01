//! Persistent stdio MCP sessions with response demultiplexing.
//!
//! A background reader task routes responses to per-request oneshot
//! channels, so any number of callers can have requests in flight
//! concurrently — the session mutex only guards (re)spawning and the brief
//! stdin writes. When the child dies or the pipe breaks, the session is
//! marked dead, pending requests fail with the captured stderr tail, and the
//! next call respawns a fresh process.

use std::collections::{BTreeMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};
use tool_compiler_adapter_api::process::command_candidates;

use crate::{
    DEPTH_ENV_VAR, McpClient, McpClientError, McpServerConfig, McpToolCall, McpToolInfo,
    McpToolResult, McpTransport, check_protocol_version, initialize_request,
    initialized_notification, parse_tools_list, tool_call_request, tools_list_request,
};

const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60_000;
const STDERR_TAIL_LINES: usize = 40;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Stdio MCP client with a persistent, self-healing session.
#[derive(Clone)]
pub struct McpStdioClient {
    config: McpServerConfig,
    next_id: Arc<AtomicU64>,
    session: Arc<Mutex<Option<Arc<Session>>>>,
}

struct Session {
    stdin: Mutex<ChildStdin>,
    pending: StdMutex<BTreeMap<u64, oneshot::Sender<Result<Value, McpClientError>>>>,
    stderr_tail: Arc<StdMutex<VecDeque<String>>>,
    child: StdMutex<Option<Child>>,
    dead: AtomicBool,
    reader: StdMutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(mut reader) = self.reader.lock()
            && let Some(handle) = reader.take()
        {
            handle.abort();
        }
    }
}

impl McpStdioClient {
    /// Creates a client for `config`; the server is spawned lazily.
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            next_id: Default::default(),
            session: Default::default(),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn request_timeout(&self) -> Duration {
        Duration::from_millis(
            self.config
                .request_timeout_ms
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
        )
    }

    /// Gracefully shuts the session down: close stdin, give the child a
    /// grace period, then kill it.
    pub async fn shutdown(&self) {
        let session = self.session.lock().await.take();
        if let Some(session) = session {
            session.dead.store(true, Ordering::SeqCst);
            let child = session
                .child
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .take();
            if let Some(mut child) = child {
                drop(session); // closes stdin via Session drop order
                let _ = tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await;
                let _ = child.start_kill();
            }
        }
    }

    async fn session(&self) -> Result<Arc<Session>, McpClientError> {
        let mut slot = self.session.lock().await;
        if let Some(session) = slot.as_ref()
            && !session.dead.load(Ordering::SeqCst)
        {
            return Ok(session.clone());
        }
        *slot = None;

        let session = start_session(&self.config, self.next_id()).await?;
        *slot = Some(session.clone());
        Ok(session)
    }

    async fn request(&self, id: u64, message: Value) -> Result<Value, McpClientError> {
        let session = self.session().await?;
        let receiver = session.register(id);
        session.write(message).await?;
        session
            .await_response(receiver, self.request_timeout(), id)
            .await
    }
}

impl Session {
    fn register(&self, id: u64) -> oneshot::Receiver<Result<Value, McpClientError>> {
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(id, sender);
        receiver
    }

    fn unregister(&self, id: u64) {
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&id);
    }

    async fn write(&self, message: Value) -> Result<(), McpClientError> {
        let line = serde_json::to_vec(&message)
            .map_err(|error| McpClientError::Protocol(error.to_string()))?;
        let mut stdin = self.stdin.lock().await;
        let write = async {
            stdin.write_all(&line).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        };
        write.await.map_err(|error| {
            self.dead.store(true, Ordering::SeqCst);
            McpClientError::Transport(format!("{error}{}", self.stderr_suffix()))
        })
    }

    async fn await_response(
        &self,
        receiver: oneshot::Receiver<Result<Value, McpClientError>>,
        timeout: Duration,
        id: u64,
    ) -> Result<Value, McpClientError> {
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(McpClientError::Transport(format!(
                "server closed the connection{}",
                self.stderr_suffix()
            ))),
            Err(_) => {
                // One slow tool call is not a dead server: drop the pending
                // slot but keep the session alive.
                self.unregister(id);
                Err(McpClientError::Timeout(timeout.as_millis() as u64))
            }
        }
    }

    fn stderr_suffix(&self) -> String {
        let tail = self
            .stderr_tail
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if tail.is_empty() {
            String::new()
        } else {
            format!(
                "; server stderr tail:\n{}",
                tail.iter().cloned().collect::<Vec<_>>().join("\n")
            )
        }
    }
}

#[async_trait]
impl McpClient for McpStdioClient {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        let id = self.next_id();
        self.request(id, tool_call_request(id, name, arguments))
            .await
    }

    async fn call_tools(
        &self,
        calls: Vec<McpToolCall>,
    ) -> Result<Vec<McpToolResult>, McpClientError> {
        let session = self.session().await?;
        let timeout = self.request_timeout();

        // Pipeline: register + write every request first, then await all.
        let mut receivers = Vec::with_capacity(calls.len());
        for call in &calls {
            let id = self.next_id();
            let receiver = session.register(id);
            session
                .write(tool_call_request(id, &call.name, call.arguments.clone()))
                .await?;
            receivers.push((call.node.clone(), id, receiver));
        }

        let mut results = Vec::with_capacity(receivers.len());
        for (node, id, receiver) in receivers {
            let result = session.await_response(receiver, timeout, id).await?;
            results.push(McpToolResult { node, result });
        }
        Ok(results)
    }

    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpClientError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let id = self.next_id();
            let result = self
                .request(id, tools_list_request(id, cursor.as_deref()))
                .await?;
            let (mut page, next) = parse_tools_list(&result);
            tools.append(&mut page);
            match next {
                Some(next) => cursor = Some(next),
                None => return Ok(tools),
            }
        }
    }
}

async fn start_session(
    config: &McpServerConfig,
    init_id: u64,
) -> Result<Arc<Session>, McpClientError> {
    let McpTransport::Stdio {
        command,
        args,
        env,
        env_clear,
        inherit,
    } = &config.transport
    else {
        return Err(McpClientError::Transport(
            "McpStdioClient requires a stdio transport".into(),
        ));
    };

    let mut child = spawn_stdio(command, args, env, *env_clear, inherit)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| McpClientError::Transport("missing child stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| McpClientError::Transport("missing child stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| McpClientError::Transport("missing child stderr".into()))?;

    let stderr_tail = Arc::new(StdMutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
    let stderr_sink = stderr_tail.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut tail = stderr_sink
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if tail.len() >= STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    });

    let session = Arc::new(Session {
        stdin: Mutex::new(stdin),
        pending: StdMutex::new(BTreeMap::new()),
        stderr_tail,
        child: StdMutex::new(Some(child)),
        dead: AtomicBool::new(false),
        reader: StdMutex::new(None),
    });

    let reader_session = session.clone();
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => reader_session.route_line(&line).await,
                Ok(None) | Err(_) => break,
            }
        }
        reader_session.dead.store(true, Ordering::SeqCst);
        let mut pending = reader_session
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for (_, sender) in std::mem::take(&mut *pending) {
            let _ = sender.send(Err(McpClientError::Transport(format!(
                "server closed stdout{}",
                reader_session.stderr_suffix()
            ))));
        }
    });
    *session
        .reader
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(reader);

    // Handshake through the same demultiplexed path.
    let receiver = session.register(init_id);
    session.write(initialize_request(init_id)).await?;
    let init_result = session
        .await_response(
            receiver,
            Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS),
            init_id,
        )
        .await?;
    check_protocol_version(&init_result)?;
    session.write(initialized_notification()).await?;

    Ok(session)
}

impl Session {
    async fn route_line(&self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return; // Non-JSON noise on stdout: skip, never corrupt a call.
        };

        let id = message.get("id").and_then(Value::as_u64);
        let is_request = message.get("method").is_some();

        match (id, is_request) {
            // Server-to-client request: answer method-not-found politely.
            (Some(id), true) => {
                let _ = self
                    .write(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": "client handles no server requests" }
                    }))
                    .await;
            }
            // Notification (no id): logging/progress — skip.
            (None, _) => {}
            // Response: route to its caller.
            (Some(id), false) => {
                let sender = self
                    .pending
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&id);
                if let Some(sender) = sender {
                    let _ = sender.send(parse_response(message));
                }
            }
        }
    }
}

fn parse_response(message: Value) -> Result<Value, McpClientError> {
    if let Some(error) = message.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32000);
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown MCP error")
            .to_owned();
        return Err(McpClientError::Server {
            code,
            message: text,
        });
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| McpClientError::Protocol("response missing result".into()))
}

fn spawn_stdio(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    env_clear: bool,
    inherit: &[String],
) -> Result<Child, McpClientError> {
    let depth = std::env::var(DEPTH_ENV_VAR)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let mut last_error = None;

    for candidate in command_candidates(command) {
        let mut builder = Command::new(&candidate);
        builder.args(args);
        if env_clear {
            builder.env_clear();
            for key in inherit {
                if let Ok(value) = std::env::var(key) {
                    builder.env(key, value);
                }
            }
        }
        builder
            .envs(env)
            .env(DEPTH_ENV_VAR, (depth + 1).to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        match builder.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => last_error = Some(error),
            Err(error) => return Err(McpClientError::Transport(error.to_string())),
        }
    }

    Err(McpClientError::Transport(
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| format!("program not found: {command}")),
    ))
}
