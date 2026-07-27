//! `nostdb catalog add|remove|list`.
//!
//! The catalog belongs to `nostdb-server`, which owns its contract, its validation, and its
//! serialized write. This module is the command surface over it and holds none of that: it parses
//! arguments, calls the shared type, and renders the result.
//!
//! # Why this writes the catalog directly rather than asking the daemon
//!
//! `catalog_version` 1 section 5 requires a write to be a complete replacement moved into place,
//! and says two processes may attempt one at once with the last complete write winning. That makes
//! a direct write safe, and it is the only design that keeps `catalog add` working when no daemon
//! is running — which matters, because registering a name is exactly what someone does *before*
//! starting one.

use std::io::Write;
use std::path::{Path, PathBuf};

use nostdb_server::catalog::{Catalog, Error as CatalogError};

use crate::exit::ExitClass;
use crate::output::Format;

/// A parsed `catalog` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Register a name for a database.
    Add {
        /// The name to register.
        name: String,
        /// The database the name refers to.
        path: PathBuf,
    },
    /// Remove a name.
    Remove {
        /// The name to remove.
        name: String,
    },
    /// Report every registered name.
    List {
        /// How to render the report.
        format: Format,
    },
}

/// Parses `catalog ...`, or reports the usage that was broken.
///
/// # Errors
///
/// Returns the usage message for a malformed invocation, which the caller reports as exit class 2.
pub fn parse(arguments: &[&str]) -> Result<Action, String> {
    let mut rest = arguments.iter().copied();
    match rest.next() {
        Some("add") => {
            let name = rest
                .next()
                .ok_or("catalog add needs a NAME and a PATH")?
                .to_owned();
            let path = rest.next().ok_or("catalog add needs a PATH")?;
            if rest.next().is_some() {
                return Err("catalog add takes exactly a NAME and a PATH".to_owned());
            }
            Ok(Action::Add {
                name,
                path: PathBuf::from(path),
            })
        }
        Some("remove") => {
            let name = rest.next().ok_or("catalog remove needs a NAME")?.to_owned();
            if rest.next().is_some() {
                return Err("catalog remove takes exactly a NAME".to_owned());
            }
            Ok(Action::Remove { name })
        }
        Some("list") => {
            let remaining: Vec<&str> = rest.collect();
            let format = match remaining.as_slice() {
                [] => Format::Table,
                ["--format", name] => Format::from_text(name).ok_or_else(|| {
                    format!(
                        "unknown format {name}; expected one of {}",
                        Format::NAMES.join(", ")
                    )
                })?,
                _ => return Err("catalog list takes an optional --format FORMAT".to_owned()),
            };
            Ok(Action::List { format })
        }
        Some(other) => Err(format!(
            "unknown catalog action {other}; expected add, remove, or list"
        )),
        None => Err("catalog needs an action: add, remove, or list".to_owned()),
    }
}

/// Runs a `catalog` action against the catalog at `path`.
pub fn execute(
    action: &Action,
    catalog_path: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitClass {
    let mut catalog = match Catalog::load(catalog_path) {
        Ok(catalog) => catalog,
        Err(error) => return report(&error, err),
    };

    match action {
        Action::Add { name, path } => {
            // An absolute path is the catalog contract's rule, and the caller is far more likely to
            // type a relative one. Resolving it here rather than refusing is the difference between
            // a command that works and one that is technically correct.
            let absolute = match absolute_path(path) {
                Ok(absolute) => absolute,
                Err(message) => {
                    let _ = writeln!(err, "{message}");
                    return ExitClass::Io;
                }
            };

            if let Err(rejection) = catalog.insert(name, &absolute) {
                let _ = writeln!(err, "{rejection}");
                return ExitClass::Validation;
            }
            if let Err(error) = catalog.store(catalog_path) {
                return report(&error, err);
            }
            let _ = writeln!(out, "{name} -> {}", absolute.display());
            ExitClass::Success
        }

        Action::Remove { name } => {
            if !catalog.remove(name) {
                // Not an error class of its own: the catalog does not hold the name, which is a
                // validation failure against what the caller asked for.
                let _ = writeln!(err, "the catalog holds no database named {name}");
                return ExitClass::Validation;
            }
            if let Err(error) = catalog.store(catalog_path) {
                return report(&error, err);
            }
            let _ = writeln!(out, "removed {name}");
            ExitClass::Success
        }

        Action::List { format } => {
            let entries: Vec<(&str, &Path)> = catalog
                .names()
                .filter_map(|name| catalog.get(name).map(|entry| (name, entry.path())))
                .collect();

            match format {
                Format::Json => {
                    let rendered: Vec<serde_json::Value> = entries
                        .iter()
                        .map(|(name, path)| {
                            serde_json::json!({ "name": name, "path": path.display().to_string() })
                        })
                        .collect();
                    let _ = writeln!(out, "{}", serde_json::json!({ "databases": rendered }));
                }
                _ => {
                    for (name, path) in &entries {
                        let _ = writeln!(out, "{name}\t{}", path.display());
                    }
                }
            }
            ExitClass::Success
        }
    }
}

/// The catalog's own failures, mapped to an exit class.
///
/// A malformed catalog is class 3 and an unreadable one is class 9, because the fixes differ: one
/// is edited and the other is a filesystem problem.
fn report(error: &CatalogError, err: &mut dyn Write) -> ExitClass {
    let _ = writeln!(err, "{error}");
    match error {
        CatalogError::Rejected(_) => ExitClass::Validation,
        CatalogError::Io(_) => ExitClass::Io,
    }
}

/// Resolves a path against the working directory without requiring it to exist.
///
/// `std::fs::canonicalize` would require it, and the catalog contract's section 1.3 is explicit
/// that registering a name for a database on an unmounted disk is legitimate.
fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let working = std::env::current_dir().map_err(|error| {
        format!(
            "cannot resolve {} against the working directory: {error}",
            path.display()
        )
    })?;
    Ok(working.join(path))
}

#[cfg(test)]
mod tests {
    use super::{Action, parse};
    use crate::output::Format;
    use std::path::PathBuf;

    fn args<'a>(values: &'a [&'a str]) -> &'a [&'a str] {
        values
    }

    #[test]
    fn add_takes_a_name_and_a_path() {
        assert_eq!(
            parse(args(&["add", "work", "./db.nostdb"])).expect("parsed"),
            Action::Add {
                name: "work".to_owned(),
                path: PathBuf::from("./db.nostdb"),
            }
        );
    }

    #[test]
    fn add_without_a_path_is_a_usage_error() {
        assert!(parse(args(&["add", "work"])).is_err());
        assert!(parse(args(&["add"])).is_err());
    }

    #[test]
    fn add_refuses_a_third_argument_rather_than_ignoring_it() {
        // Silently dropping an argument is how a caller ends up believing they configured
        // something they did not.
        assert!(parse(args(&["add", "work", "./a", "./b"])).is_err());
    }

    #[test]
    fn remove_takes_exactly_a_name() {
        assert_eq!(
            parse(args(&["remove", "work"])).expect("parsed"),
            Action::Remove {
                name: "work".to_owned()
            }
        );
        assert!(parse(args(&["remove"])).is_err());
        assert!(parse(args(&["remove", "work", "spare"])).is_err());
    }

    #[test]
    fn list_defaults_to_the_table_format_and_accepts_json() {
        assert_eq!(
            parse(args(&["list"])).expect("parsed"),
            Action::List {
                format: Format::Table
            }
        );
        assert_eq!(
            parse(args(&["list", "--format", "json"])).expect("parsed"),
            Action::List {
                format: Format::Json
            }
        );
    }

    #[test]
    fn an_unknown_action_names_the_ones_that_exist() {
        let message = parse(args(&["vacuum"])).expect_err("refused");
        assert!(message.contains("add"), "{message}");
        assert!(message.contains("remove"), "{message}");
        assert!(message.contains("list"), "{message}");
    }
}
