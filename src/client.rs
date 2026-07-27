//! The daemon client.
//!
//! `nostdb-cli/AGENTS.md` permits this repository to own the daemon client and prohibits it from
//! owning an IPC transport. Both hold here: the conversation is driven from this module, and the
//! framing and message shapes come from `nostdb-server`, which owns the protocol.
//!
//! This is what `--database @name`, `server start`, and `server stop` all needed. They were three
//! separate gaps that turned out to be one.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nostdb_server::frame::{self, FrameError};
use nostdb_server::message;
use serde_json::{Value, json};

use crate::exit::ExitClass;

/// Why talking to the daemon failed.
#[derive(Debug)]
pub enum ClientError {
    /// No daemon is listening.
    ///
    /// Kept apart from every other failure because it is the one with an obvious fix, and the
    /// message says what it is.
    NotRunning(String),
    /// The endpoint could not be located.
    NoEndpoint(String),
    /// The transport failed mid-conversation.
    Transport(String),
    /// The daemon refused something.
    ///
    /// Carries the whole reply, because the daemon already said which rule or code applied and
    /// restating it here would be a second vocabulary for one failure.
    Refused(Value),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning(endpoint) => write!(
                formatter,
                "no daemon is listening on {endpoint}; start one with `nostdb server start`"
            ),
            Self::NoEndpoint(detail) | Self::Transport(detail) => formatter.write_str(detail),
            Self::Refused(reply) => {
                let detail = reply
                    .get("detail")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        reply
                            .pointer("/diagnostics/0/message")
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("the daemon refused the request");
                match reply.get("rule").or_else(|| reply.get("code")) {
                    Some(named) => write!(formatter, "{}: {detail}", named.as_str().unwrap_or("")),
                    None => formatter.write_str(detail),
                }
            }
        }
    }
}

impl ClientError {
    /// The exit class this failure maps to.
    ///
    /// A daemon that is not running is class 5, unavailable: the request was well formed and the
    /// thing it needed was not there. A refusal the daemon named is class 3, because the daemon
    /// read it and said what was wrong with it.
    #[must_use]
    pub const fn class(&self) -> ExitClass {
        match self {
            Self::NotRunning(_) => ExitClass::Unavailable,
            Self::NoEndpoint(_) | Self::Transport(_) => ExitClass::Io,
            Self::Refused(_) => ExitClass::Validation,
        }
    }
}

/// A connection to the daemon, past the handshake.
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
    version: u64,
    session: Option<String>,
    next_id: u64,
    max_frame_bytes: u32,
}

impl Client {
    /// Connects to this user's daemon and completes the handshake.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::NotRunning`] when nothing is listening, and
    /// [`ClientError::Refused`] when the daemon and this build share no protocol version.
    pub fn connect() -> Result<Self, ClientError> {
        let address = endpoint()?;
        let stream = UnixStream::connect(&address).map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) {
                ClientError::NotRunning(address.display().to_string())
            } else {
                ClientError::Transport(format!("cannot reach {}: {error}", address.display()))
            }
        })?;

        let mut client = Self {
            stream,
            version: 0,
            session: None,
            next_id: 1,
            max_frame_bytes: frame::MINIMUM_MAXIMUM_FRAME_BYTES,
        };

        client.send(&json!({
            "message": "hello",
            "client": "nostdb-cli",
            "supported_versions": message::SUPPORTED_VERSIONS,
        }))?;
        let welcome = client.receive()?;

        // A refusal names the versions the daemon has, which is the actionable part. It is passed
        // through rather than summarized, so the caller sees what the daemon said.
        if welcome.get("message").and_then(Value::as_str) != Some("welcome") {
            return Err(ClientError::Refused(welcome));
        }
        client.version = welcome
            .get("server_protocol_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| ClientError::Transport("the welcome states no version".to_owned()))?;
        Ok(client)
    }

    /// Opens the connection's session on a catalog name.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Refused`] when the daemon refuses, which includes a name the catalog
    /// does not hold and a name that is really a path.
    pub fn open_session(&mut self, database: &str) -> Result<(), ClientError> {
        let reply = self.request(json!({
            "operation": "open_session",
            "database": database,
        }))?;
        self.session = reply
            .pointer("/result/session_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(())
    }

    /// Runs one statement and returns the result envelope the Engine built.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Refused`] carrying whatever the daemon reported, including the
    /// Engine's own diagnostic code for a query the Engine refused.
    pub fn query(&mut self, statement: &str) -> Result<Value, ClientError> {
        let mut request = json!({
            "operation": "query",
            "statement": statement,
        });
        if let Some(session) = &self.session {
            request["session_id"] = Value::String(session.clone());
        }
        let reply = self.request(request)?;
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Asks the daemon to stop.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Refused`] when the daemon declines, which it does while a
    /// transaction is open.
    pub fn shutdown(&mut self) -> Result<(), ClientError> {
        self.request(json!({ "operation": "shutdown" }))?;
        Ok(())
    }

    /// The negotiated protocol version.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Sends a request and returns its reply, refusing an `error` outcome.
    fn request(&mut self, mut body: Value) -> Result<Value, ClientError> {
        let id = format!("r{}", self.next_id);
        self.next_id += 1;
        body["server_protocol_version"] = Value::Number(self.version.into());
        body["request_id"] = Value::String(id.clone());

        self.send(&body)?;
        let reply = self.receive()?;

        // A response may arrive for any outstanding request. This client sends one at a time, so a
        // reply for another identifier means the daemon and this build disagree about the protocol
        // rather than that a reply is merely out of order.
        if reply.get("request_id").and_then(Value::as_str) != Some(id.as_str()) {
            return Err(ClientError::Transport(format!(
                "the daemon answered {:?} to request {id}",
                reply.get("request_id")
            )));
        }
        if reply.get("outcome").and_then(Value::as_str) == Some("error") {
            return Err(ClientError::Refused(reply));
        }
        Ok(reply)
    }

    fn send(&mut self, value: &Value) -> Result<(), ClientError> {
        let body = serde_json::to_string(value)
            .map_err(|error| ClientError::Transport(format!("{error}")))?;
        frame::write_frame(&mut self.stream, &body, self.max_frame_bytes).map_err(transport)
    }

    fn receive(&mut self) -> Result<Value, ClientError> {
        let body = frame::read_frame(&mut self.stream, self.max_frame_bytes).map_err(transport)?;
        serde_json::from_str(&body).map_err(|error| {
            ClientError::Transport(format!("the daemon sent unreadable JSON: {error}"))
        })
    }
}

fn transport(error: FrameError) -> ClientError {
    match error {
        FrameError::Closed => ClientError::Transport("the daemon closed the connection".to_owned()),
        other => ClientError::Transport(format!("{other}")),
    }
}

fn endpoint() -> Result<PathBuf, ClientError> {
    nostdb_server::endpoint::address().map_err(|error| ClientError::NoEndpoint(format!("{error}")))
}

/// Starts the daemon in the background and waits for it to be reachable.
///
/// The daemon is started as `binary server run` rather than by finding `nostdb-server` on the PATH.
/// That keeps `nostdb server start` working from a build directory and from an installation where
/// only one of the two binaries happens to be on the PATH, and it means the daemon a caller starts
/// always matches the client that started it.
///
/// `binary` is a parameter rather than `current_exe()` because `current_exe()` is only the `nostdb`
/// binary when a person ran it. Under a test harness it is the test binary, which was found by
/// spawning it and watching it exit 0 after treating `server run` as a test filter.
///
/// # Errors
///
/// Returns an error when spawning fails, when the daemon exits immediately, or when it does not
/// become reachable before `timeout`.
pub fn start_daemon(
    binary: &std::path::Path,
    timeout: Duration,
    err: &mut dyn Write,
) -> Result<PathBuf, String> {
    if nostdb_server::is_running().unwrap_or(false) {
        // Section 2.1: starting something already started is what the caller wanted.
        return endpoint().map_err(|error| format!("{error}"));
    }

    let mut child = std::process::Command::new(binary)
        .args(["server", "run"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", binary.display()))?;

    // Waiting for the endpoint rather than assuming it appeared. A start that returned immediately
    // would have the next command fail against a daemon that had not finished binding.
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if nostdb_server::is_running().unwrap_or(false) {
            return endpoint().map_err(|error| format!("{error}"));
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("the daemon exited immediately with {status}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let _ = writeln!(err, "the daemon did not become reachable; stopping it");
    let _ = child.kill();
    Err(format!(
        "the daemon did not become reachable within {} milliseconds",
        timeout.as_millis()
    ))
}

#[cfg(test)]
mod tests {
    use super::ClientError;
    use crate::exit::ExitClass;
    use serde_json::json;

    #[test]
    fn a_daemon_that_is_not_running_is_unavailable_rather_than_an_io_failure() {
        // The two have different fixes: start a daemon, or investigate a filesystem problem.
        let error = ClientError::NotRunning("/run/nostdb.sock".to_owned());
        assert_eq!(error.class(), ExitClass::Unavailable);
        assert!(
            error.to_string().contains("nostdb server start"),
            "the message must say how to fix it: {error}"
        );
    }

    #[test]
    fn a_refusal_reports_what_the_daemon_named_rather_than_a_second_vocabulary() {
        let error = ClientError::Refused(json!({
            "outcome": "error",
            "rule": "unknown_session",
            "detail": "this connection has no open session",
        }));
        assert_eq!(error.class(), ExitClass::Validation);
        let message = error.to_string();
        assert!(message.contains("unknown_session"), "{message}");
        assert!(message.contains("no open session"), "{message}");
    }

    #[test]
    fn a_refusal_carrying_an_engine_diagnostic_reports_that_message() {
        let error = ClientError::Refused(json!({
            "outcome": "error",
            "diagnostics": [{ "code": "CYPHER_SEMANTIC_ERROR", "message": "no such thing" }],
        }));
        assert!(error.to_string().contains("no such thing"), "{error}");
    }
}
