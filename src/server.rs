//! `nostdb server start|status|stop|run`.
//!
//! The daemon is `nostdb-server`. This module drives it and implements none of it.
//!
//! `start` spawns this binary as `server run` and waits for the endpoint to appear rather than
//! assuming it did. `stop` asks the daemon over the protocol. `run` stays in the foreground, which
//! is what a service manager and a debugging session both want.
//!
//! # Why `status` asks the lock rather than the socket
//!
//! Section 2.1 forbids treating a leftover socket file as proof that a daemon is running, because a
//! killed process leaves one behind. The lock is what answers the question, and `nostdb-server`
//! exposes exactly that.

use std::io::Write;

use crate::exit::ExitClass;

/// A parsed `server` invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start the daemon in the background, or report the one already running.
    Start,
    /// Report whether a daemon is running for this user.
    Status,
    /// Ask the running daemon to stop.
    Stop,
    /// Run the daemon in the foreground.
    Run,
}

/// Parses `server ...`.
///
/// A bare `server` is `start`, which section 2.1 makes the alias.
///
/// # Errors
///
/// Returns the usage message for an unknown action.
pub fn parse(arguments: &[&str]) -> Result<Action, String> {
    match arguments {
        [] | ["start"] => Ok(Action::Start),
        ["status"] => Ok(Action::Status),
        ["stop"] => Ok(Action::Stop),
        ["run"] => Ok(Action::Run),
        [other, ..] => Err(format!(
            "unknown server action {other}; expected start, status, stop, or run"
        )),
    }
}

/// Runs a `server` action.
pub fn execute(action: Action, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    match action {
        Action::Status => match nostdb_server::is_running() {
            Ok(true) => {
                let _ = writeln!(out, "running");
                ExitClass::Success
            }
            Ok(false) => {
                let _ = writeln!(out, "not running");
                ExitClass::Success
            }
            Err(error) => {
                let _ = writeln!(err, "{error}");
                ExitClass::Io
            }
        },

        Action::Run => match nostdb_server::start() {
            Ok(nostdb_server::Started::AlreadyRunning { address, code }) => {
                // Section 2.1: starting something already started is what the caller wanted, so
                // this is a success that reports the existing endpoint.
                let _ = writeln!(out, "{code}: already listening on {}", address.display());
                ExitClass::Success
            }
            Ok(nostdb_server::Started::Running {
                address, listener, ..
            }) => {
                let _ = writeln!(out, "listening on {}", address.display());
                let catalog_path = match nostdb_server::catalog::Catalog::default_path() {
                    Ok(path) => path,
                    Err(error) => {
                        let _ = writeln!(err, "{error}");
                        return ExitClass::Io;
                    }
                };
                match nostdb_server::accept_until_shutdown(
                    &listener,
                    &catalog_path,
                    nostdb_server::serve::Limits::default(),
                ) {
                    Ok(()) => ExitClass::Success,
                    Err(error) => {
                        let _ = writeln!(err, "{error}");
                        ExitClass::Io
                    }
                }
            }
            Err(error) => {
                let _ = writeln!(err, "{error}");
                ExitClass::Io
            }
        },

        Action::Start => {
            let binary = match std::env::current_exe() {
                Ok(binary) => binary,
                Err(error) => {
                    let _ = writeln!(
                        err,
                        "cannot locate this binary to start the daemon: {error}"
                    );
                    return ExitClass::Io;
                }
            };
            match crate::client::start_daemon(&binary, std::time::Duration::from_secs(10), err) {
                Ok(address) => {
                    let _ = writeln!(out, "listening on {}", address.display());
                    ExitClass::Success
                }
                Err(message) => {
                    let _ = writeln!(err, "{message}");
                    ExitClass::Io
                }
            }
        }

        Action::Stop => match crate::client::Client::connect() {
            Ok(mut client) => match client.shutdown() {
                Ok(()) => {
                    let _ = writeln!(out, "stopped");
                    ExitClass::Success
                }
                Err(error) => {
                    let _ = writeln!(err, "{error}");
                    error.class()
                }
            },
            // Stopping something that is not running is not a failure of the request: there is
            // nothing left to do, which is what the caller wanted.
            Err(crate::client::ClientError::NotRunning(_)) => {
                let _ = writeln!(out, "not running");
                ExitClass::Success
            }
            Err(error) => {
                let _ = writeln!(err, "{error}");
                error.class()
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, parse};

    #[test]
    fn a_bare_server_is_start() {
        // Section 2.1 makes `nostdb server` an alias for `nostdb server start`.
        assert_eq!(parse(&[]).expect("parsed"), Action::Start);
        assert_eq!(parse(&["start"]).expect("parsed"), Action::Start);
    }

    #[test]
    fn each_published_action_parses() {
        assert_eq!(parse(&["status"]).expect("parsed"), Action::Status);
        assert_eq!(parse(&["stop"]).expect("parsed"), Action::Stop);
        assert_eq!(parse(&["run"]).expect("parsed"), Action::Run);
    }

    #[test]
    fn an_unknown_action_names_the_ones_that_exist() {
        let message = parse(&["restart"]).expect_err("refused");
        for action in ["start", "status", "stop", "run"] {
            assert!(message.contains(action), "{message}");
        }
    }
}
