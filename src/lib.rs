//! The NostDB command surface.
//!
//! This crate implements no database behavior. Every command is argument parsing, a call
//! into `nostdb-core`, and a rendering of what came back. Where a command appears to need
//! a parser, a storage engine, or a query engine, it needs a public Core API instead.
//!
//! # Why `run` takes writers
//!
//! [`run`] writes through `&mut dyn Write` rather than to the process streams, and
//! returns an [`ExitClass`] rather than exiting. A test can then drive the whole command
//! surface in-process and assert both the output and the exit class, which is what makes
//! the normative exit classes testable at all. [`main`](../src/main.rs) is the only place
//! that touches the real streams or the process status.
//!
//! # Data and diagnostics are separate
//!
//! Anything a caller would pipe goes to `out`. Anything explaining what happened goes to
//! `err`. That holds for every command, so a machine-readable mode never has to strip
//! commentary out of its input.

#![forbid(unsafe_code)]

pub mod command;
pub mod exit;

pub use exit::ExitClass;

use std::io::Write;
use std::path::PathBuf;

/// The product name, as reported by `--version`.
pub const PRODUCT: &str = "nostdb";

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parses `arguments` and runs the command they name.
///
/// `arguments` excludes the program name. Returns the exit class the process should
/// report; nothing here exits, panics for an ordinary error, or writes to a stream it was
/// not handed.
pub fn run(arguments: &[String], out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    match Invocation::parse(arguments) {
        Err(usage) => {
            let _ = writeln!(err, "{usage}");
            let _ = writeln!(err, "Run `nostdb help` for the command surface.");
            ExitClass::Usage
        }
        Ok(invocation) => invocation.execute(out, err),
    }
}

/// A parsed invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    /// `nostdb help [COMMAND]`
    Help {
        /// The command to describe, or none for the summary.
        topic: Option<String>,
    },
    /// `nostdb --version [--json]`
    Version {
        /// Whether to report machine-readable JSON.
        json: bool,
    },
    /// `nostdb init [PATH]`
    Init {
        /// The directory to configure.
        path: PathBuf,
    },
    /// `nostdb check TARGET`
    Check {
        /// The `.nost` or `.nostdb` file to validate.
        target: PathBuf,
    },
    /// `nostdb convert INPUT OUTPUT`
    Convert {
        /// The file to read.
        input: PathBuf,
        /// The file to write.
        output: PathBuf,
    },
    /// `nostdb export --nost [PATH]`
    Export {
        /// Where to start looking for the active project.
        from: PathBuf,
    },
}

/// Why an invocation could not be understood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError(String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn usage(message: impl Into<String>) -> UsageError {
    UsageError(message.into())
}

/// Rejects an argument that looks like a flag where a path belongs.
///
/// Without this a mistyped flag becomes a file name, and the command fails much later
/// with a confusing "no such file" rather than at the point of the mistake.
fn positional(value: &str, what: &str) -> Result<PathBuf, UsageError> {
    if value.starts_with('-') {
        return Err(usage(format!(
            "expected {what}, found the option `{value}`"
        )));
    }
    Ok(PathBuf::from(value))
}

impl Invocation {
    /// Parses arguments, excluding the program name.
    ///
    /// # Errors
    ///
    /// Returns a [`UsageError`] naming what was wrong for an unknown command, a missing
    /// operand, an unexpected operand, or an unknown option.
    pub fn parse(arguments: &[String]) -> Result<Self, UsageError> {
        let mut rest = arguments.iter().map(String::as_str);
        let Some(first) = rest.next() else {
            return Ok(Self::Help { topic: None });
        };
        let remainder: Vec<&str> = rest.collect();

        match first {
            "--version" | "-V" | "version" => {
                let json = match remainder.as_slice() {
                    [] => false,
                    ["--json"] => true,
                    [other, ..] => {
                        return Err(usage(format!("`version` does not take `{other}`")));
                    }
                };
                Ok(Self::Version { json })
            }
            "--help" | "-h" | "help" => match remainder.as_slice() {
                [] => Ok(Self::Help { topic: None }),
                [topic] => Ok(Self::Help {
                    topic: Some((*topic).to_owned()),
                }),
                [_, extra, ..] => Err(usage(format!("`help` takes one command, found `{extra}`"))),
            },
            "init" => match remainder.as_slice() {
                [] => Ok(Self::Init {
                    path: PathBuf::from("."),
                }),
                [path] => Ok(Self::Init {
                    path: positional(path, "a directory")?,
                }),
                [_, extra, ..] => Err(usage(format!("`init` takes one path, found `{extra}`"))),
            },
            "check" => match remainder.as_slice() {
                [] => Err(usage("`check` needs a target: `nostdb check TARGET`")),
                [target] => Ok(Self::Check {
                    target: positional(target, "a target")?,
                }),
                [_, extra, ..] => Err(usage(format!("`check` takes one target, found `{extra}`"))),
            },
            "convert" => match remainder.as_slice() {
                [] | [_] => Err(usage(
                    "`convert` needs two paths: `nostdb convert INPUT OUTPUT`",
                )),
                [input, output] => Ok(Self::Convert {
                    input: positional(input, "an input path")?,
                    output: positional(output, "an output path")?,
                }),
                [_, _, extra, ..] => {
                    Err(usage(format!("`convert` takes two paths, found `{extra}`")))
                }
            },
            "export" => {
                // `--nost` is required rather than assumed. It is the only representation
                // this build exports, and requiring it now keeps adding another from
                // silently changing what a bare `export` means.
                let mut from = PathBuf::from(".");
                let mut saw_nost = false;
                let mut positionals = 0_usize;
                for argument in &remainder {
                    match *argument {
                        "--nost" => saw_nost = true,
                        other if other.starts_with('-') => {
                            return Err(usage(format!("`export` does not take `{other}`")));
                        }
                        other => {
                            positionals += 1;
                            if positionals > 1 {
                                return Err(usage(format!(
                                    "`export` takes one path, found `{other}`"
                                )));
                            }
                            from = PathBuf::from(other);
                        }
                    }
                }
                if !saw_nost {
                    return Err(usage("`export` needs `--nost`: `nostdb export --nost`"));
                }
                Ok(Self::Export { from })
            }
            other if other.starts_with('-') => Err(usage(format!("unknown option `{other}`"))),
            other => Err(usage(format!("unknown command `{other}`"))),
        }
    }

    /// Runs this invocation.
    fn execute(self, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
        match self {
            Self::Help { topic } => command::help(topic.as_deref(), out, err),
            Self::Version { json } => command::version(json, out),
            Self::Init { path } => command::init(&path, out, err),
            Self::Check { target } => command::check(&target, out, err),
            Self::Convert { input, output } => command::convert(&input, &output, out, err),
            Self::Export { from } => command::export(&from, out, err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<Invocation, UsageError> {
        let arguments: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        Invocation::parse(&arguments)
    }

    #[test]
    fn no_arguments_shows_the_summary() {
        assert_eq!(parse(""), Ok(Invocation::Help { topic: None }));
    }

    #[test]
    fn help_takes_an_optional_topic() {
        assert_eq!(parse("help"), Ok(Invocation::Help { topic: None }));
        assert_eq!(
            parse("help convert"),
            Ok(Invocation::Help {
                topic: Some("convert".to_owned())
            })
        );
        assert!(parse("help convert extra").is_err());
    }

    #[test]
    fn version_accepts_only_the_json_flag() {
        assert_eq!(parse("--version"), Ok(Invocation::Version { json: false }));
        assert_eq!(
            parse("--version --json"),
            Ok(Invocation::Version { json: true })
        );
        assert!(parse("--version --xml").is_err());
    }

    #[test]
    fn init_defaults_to_the_working_directory() {
        assert_eq!(
            parse("init"),
            Ok(Invocation::Init {
                path: PathBuf::from(".")
            })
        );
        assert_eq!(
            parse("init packages/child"),
            Ok(Invocation::Init {
                path: PathBuf::from("packages/child")
            })
        );
    }

    #[test]
    fn a_missing_operand_is_a_usage_error_naming_the_form() {
        let error = parse("check").unwrap_err();
        assert!(error.to_string().contains("nostdb check TARGET"), "{error}");
        let error = parse("convert one.nost").unwrap_err();
        assert!(error.to_string().contains("INPUT OUTPUT"), "{error}");
    }

    #[test]
    fn a_flag_where_a_path_belongs_is_caught_at_the_mistake() {
        // Otherwise it becomes a file name and fails much later as "no such file".
        let error = parse("check --nost").unwrap_err();
        assert!(error.to_string().contains("--nost"), "{error}");
        assert!(error.to_string().contains("expected a target"), "{error}");
    }

    #[test]
    fn export_requires_the_nost_flag() {
        let error = parse("export").unwrap_err();
        assert!(error.to_string().contains("--nost"), "{error}");
        assert_eq!(
            parse("export --nost"),
            Ok(Invocation::Export {
                from: PathBuf::from(".")
            })
        );
        assert_eq!(
            parse("export --nost some/where"),
            Ok(Invocation::Export {
                from: PathBuf::from("some/where")
            })
        );
    }

    #[test]
    fn an_unknown_command_or_option_is_refused() {
        assert!(
            parse("frobnicate")
                .unwrap_err()
                .to_string()
                .contains("frobnicate")
        );
        assert!(
            parse("--nonsense")
                .unwrap_err()
                .to_string()
                .contains("nonsense")
        );
    }

    #[test]
    fn a_usage_error_exits_with_class_two_and_writes_nothing_to_stdout() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let class = run(&["frobnicate".to_owned()], &mut out, &mut err);
        assert_eq!(class, ExitClass::Usage);
        assert!(out.is_empty(), "usage output must not reach stdout");
        assert!(String::from_utf8_lossy(&err).contains("frobnicate"));
    }
}
