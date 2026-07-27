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

pub mod apply;
pub mod build;
pub mod catalog;
pub mod command;
pub mod exit;
pub mod link;
pub mod output;
pub mod plan;
pub mod query;
pub mod server;
pub mod sync;

pub use exit::ExitClass;
pub use output::Format;

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
    /// `nostdb link list|check|add|remove [OPERANDS] [--format FORMAT] [--project PATH]`
    Link {
        /// What to do.
        action: link::Action,
        /// How to write the report.
        format: Format,
        /// Where to start looking for the active project.
        from: PathBuf,
    },
    /// `nostdb apply FILE [--format FORMAT] [--project PATH]`
    Apply {
        /// The change set to read.
        file: PathBuf,
        /// Where to start looking for the active project.
        from: PathBuf,
        /// How to write the report.
        format: Format,
    },
    /// `nostdb build [PATH] [--format FORMAT] [--rebuild]`
    Build {
        /// Where to start looking for the active project.
        from: PathBuf,
        /// How to write the report.
        format: Format,
        /// Whether to re-read every file rather than reusing what is recorded.
        rebuild: bool,
    },
    /// `nostdb plan [PATH] [--format FORMAT]`
    Plan {
        /// Where to start looking for the active project.
        from: PathBuf,
        /// How to write the report.
        format: Format,
    },
    /// `nostdb catalog add|remove|list [OPERANDS]`
    Catalog {
        /// What to do to the catalog.
        action: crate::catalog::Action,
    },
    /// `nostdb server start|status|stop|run`
    Server {
        /// What to do to the daemon.
        action: crate::server::Action,
    },
    /// `nostdb sync [PATH]`
    Sync {
        /// Where to start looking for the active project.
        from: PathBuf,
    },
    /// `nostdb query [CYPHER] [--format FORMAT] [--project PATH]`
    Query {
        /// The statement to run, or none for the REPL.
        cypher: Option<String>,
        /// How to write the result.
        format: Format,
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
            "link" => parse_link(&remainder),
            "apply" => {
                let Some((first, rest)) = remainder.split_first() else {
                    return Err(usage("`apply` needs a change set file"));
                };
                let file = positional(first, "a change set file")?;
                let (format, from) = parse_shared_options(rest, "apply")?;
                Ok(Self::Apply { file, from, format })
            }
            "build" => {
                let rebuild = remainder.contains(&"--rebuild");
                let rest: Vec<&str> = remainder
                    .iter()
                    .copied()
                    .filter(|word| *word != "--rebuild")
                    .collect();
                let (format, from) = parse_shared_options(&rest, "build")?;
                Ok(Self::Build {
                    from,
                    format,
                    rebuild,
                })
            }
            "plan" => {
                let (format, from) = parse_shared_options(&remainder, "plan")?;
                Ok(Self::Plan { from, format })
            }
            "catalog" => crate::catalog::parse(&remainder)
                .map(|action| Self::Catalog { action })
                .map_err(usage),
            "server" => crate::server::parse(&remainder)
                .map(|action| Self::Server { action })
                .map_err(usage),
            "sync" => match remainder.as_slice() {
                [] => Ok(Self::Sync {
                    from: PathBuf::from("."),
                }),
                [path] => Ok(Self::Sync {
                    from: positional(path, "a project path")?,
                }),
                [_, extra, ..] => Err(usage(format!("`sync` takes one path, found `{extra}`"))),
            },
            "query" => parse_query(&remainder),
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
            Self::Link {
                action,
                format,
                from,
            } => link::run(&action, &from, format, out, err),
            Self::Apply { file, from, format } => apply::run(&file, &from, format, out, err),
            Self::Build {
                from,
                format,
                rebuild,
            } => build::run(&from, format, rebuild, out, err),
            Self::Plan { from, format } => plan::run(&from, format, out, err),
            Self::Catalog { action } => {
                // The catalog is per operating-system user and lives at a fixed location, so it is
                // resolved here rather than taken as an argument. A `--catalog` flag would let one
                // invocation register a name another could never see.
                match nostdb_server::catalog::Catalog::default_path() {
                    Ok(path) => catalog::execute(&action, &path, out, err),
                    Err(error) => {
                        let _ = writeln!(err, "{error}");
                        ExitClass::Io
                    }
                }
            }
            Self::Server { action } => server::execute(action, out, err),
            Self::Sync { from } => sync::run(&from, out, err),
            Self::Query {
                cypher,
                format,
                from,
            } => match cypher {
                Some(text) => query::immediate(&from, &text, format, out, err),
                None => {
                    let stdin = std::io::stdin();
                    let mut input = stdin.lock();
                    query::repl(&from, format, &mut input, out, err)
                }
            },
        }
    }
}

/// Parses `link`'s operands.
///
/// The action grammar lives in [`link::parse`], beside the actions themselves, so adding
/// one does not mean editing two files. A subcommand the product contract names and this
/// build does not implement is refused by name there, rather than falling through to
/// "unknown": a caller who typed a real command deserves to be told it is not built yet,
/// not that it does not exist.
fn parse_link(remainder: &[&str]) -> Result<Invocation, UsageError> {
    let (action, rest) = link::parse(remainder).map_err(usage)?;
    let (format, from) = parse_shared_options(rest, "link")?;
    Ok(Invocation::Link {
        action,
        format,
        from,
    })
}

/// Parses the `--format` and `--project` options both `link` and `query` accept.
fn parse_shared_options(
    remainder: &[&str],
    command: &str,
) -> Result<(Format, PathBuf), UsageError> {
    let mut format = Format::default();
    let mut from = PathBuf::from(".");
    let mut index = 0;
    while index < remainder.len() {
        let argument = remainder[index];
        let (name, inline) = argument
            .split_once('=')
            .map_or((argument, None), |(name, value)| (name, Some(value)));
        match name {
            "--format" | "--project" => {
                let value = match inline {
                    Some(value) => {
                        index += 1;
                        value
                    }
                    None => {
                        let Some(value) = remainder.get(index + 1) else {
                            return Err(usage(format!("`{name}` needs a value")));
                        };
                        index += 2;
                        value
                    }
                };
                if name == "--format" {
                    format = Format::from_text(value).ok_or_else(|| {
                        usage(format!(
                            "`{value}` is not a format; expected one of {:?}",
                            Format::NAMES
                        ))
                    })?;
                } else {
                    from = positional(value, "a project path")?;
                }
            }
            other => {
                return Err(usage(format!("`{command}` does not take `{other}`")));
            }
        }
    }
    Ok((format, from))
}

/// Parses `query`'s operands.
///
/// The statement is positional and optional; omitting it opens the REPL. Options may
/// appear before or after it, because a caller reaching for `--format` after typing a
/// long statement should not have to move it.
fn parse_query(remainder: &[&str]) -> Result<Invocation, UsageError> {
    let mut cypher: Option<String> = None;
    let mut format = Format::default();
    let mut from = PathBuf::from(".");
    let mut index = 0;

    while index < remainder.len() {
        let argument = remainder[index];
        match argument {
            "--format" | "--project" => {
                let Some(value) = remainder.get(index + 1) else {
                    return Err(usage(format!("`{argument}` needs a value")));
                };
                if argument == "--format" {
                    format = Format::from_text(value).ok_or_else(|| {
                        usage(format!(
                            "`{value}` is not a format; expected one of {:?}",
                            Format::NAMES
                        ))
                    })?;
                } else {
                    from = positional(value, "a project path")?;
                }
                index += 2;
            }
            other if other.starts_with("--format=") || other.starts_with("--project=") => {
                let (name, value) = other.split_once('=').unwrap_or((other, ""));
                if name == "--format" {
                    format = Format::from_text(value).ok_or_else(|| {
                        usage(format!(
                            "`{value}` is not a format; expected one of {:?}",
                            Format::NAMES
                        ))
                    })?;
                } else {
                    from = positional(value, "a project path")?;
                }
                index += 1;
            }
            other if other.starts_with('-') => {
                return Err(usage(format!("`query` does not take `{other}`")));
            }
            other => {
                if cypher.is_some() {
                    return Err(usage(format!(
                        "`query` takes one statement, found `{other}`"
                    )));
                }
                cypher = Some(other.to_owned());
                index += 1;
            }
        }
    }

    Ok(Invocation::Query {
        cypher,
        format,
        from,
    })
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
