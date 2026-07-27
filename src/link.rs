//! `nostdb link list` and `nostdb link check`.
//!
//! Both are read-only. `add`, `remove`, and `refresh` change state and need the
//! multi-file journal the settings contract requires for reconciling a declaration with
//! its operational entry, which is not built; they are refused with a message saying so
//! rather than half-implemented.

use crate::exit::ExitClass;
use crate::output::Format;
use nostdb_core::federation::{Federation, LinkStatus};
use nostdb_core::project::Project;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

/// What `link` was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Report every declared link and what became of it.
    List,
    /// Report the same, and fail when any link is unreachable.
    Check,
}

impl Action {
    /// Reads an action from the subcommand a caller typed.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        Some(match text {
            "list" => Self::List,
            "check" => Self::Check,
            _ => return None,
        })
    }

    /// Subcommands this build implements.
    pub const IMPLEMENTED: [&'static str; 2] = ["list", "check"];

    /// Subcommands the product contract names and this build does not implement.
    pub const DEFERRED: [&'static str; 3] = ["add", "remove", "refresh"];
}

fn global_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".nostdb").join("settings.json"))
}

fn status_json(status: &LinkStatus) -> Value {
    let mut entry = json!({
        "source": status.locator.as_str(),
        "available": status.is_available(),
        "depth": status.depth,
    });
    if let Some(alias) = &status.alias {
        entry["alias"] = json!(alias);
    }
    if let Some(declared_by) = &status.declared_by {
        entry["declared_by"] = json!(declared_by.as_str());
    }
    if let Some(unreachable) = &status.unreachable {
        entry["code"] = json!(unreachable.code().as_str());
        entry["reason"] = json!(unreachable.to_string());
    }
    entry
}

fn write_json(federation: &Federation, out: &mut dyn Write) {
    let document = json!({
        "links": federation.statuses.iter().map(status_json).collect::<Vec<_>>(),
        "summary": {
            "declared": federation.statuses.len(),
            "linked_databases_opened": federation.linked_databases_opened(),
            "partial": federation.is_partial(),
        },
    });
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&document).unwrap_or_else(|_| document.to_string())
    );
}

fn write_table(federation: &Federation, out: &mut dyn Write) {
    if federation.statuses.is_empty() {
        let _ = writeln!(out, "no links declared");
        return;
    }
    for status in &federation.statuses {
        let alias = status
            .alias
            .as_ref()
            .map_or_else(String::new, |alias| format!(" as {alias}"));
        match &status.unreachable {
            None => {
                let _ = writeln!(out, "ok         {}{alias}", status.locator);
            }
            Some(unreachable) => {
                let _ = writeln!(
                    out,
                    "{:<10} {}{alias} — {unreachable}",
                    unreachable.code().as_str(),
                    status.locator
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "\n{} declared, {} opened{}",
        federation.statuses.len(),
        federation.linked_databases_opened(),
        if federation.is_partial() {
            ", partial"
        } else {
            ""
        }
    );
}

/// Runs `nostdb link <ACTION>`.
pub fn run(
    action: Action,
    from: &Path,
    format: Format,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitClass {
    let project = match Project::discover(from, global_settings_path().as_deref()) {
        Ok(project) => project,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitClass::for_project_error(&error);
        }
    };
    let federation = match project.resolve_links() {
        Ok(federation) => federation,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitClass::for_project_error(&error);
        }
    };

    match format {
        Format::Json | Format::Jsonl => write_json(&federation, out),
        Format::Table | Format::Csv => write_table(&federation, out),
    }

    // An orphan entry is about settings rather than about reachability, so it is reported
    // whichever action ran and never changes the exit class.
    for orphan in project.orphan_link_settings(federation.root()) {
        let _ = writeln!(
            err,
            "{}: {}: {}",
            orphan.severity,
            orphan.code.as_str(),
            orphan.message
        );
    }

    match action {
        // `list` reports; it does not judge. A broken link is a fact about the workspace,
        // not a failure of the command that listed it.
        Action::List => {
            for warning in federation.warnings() {
                let _ = writeln!(
                    err,
                    "{}: {}: {}",
                    warning.severity,
                    warning.code.as_str(),
                    warning.message
                );
            }
            ExitClass::Success
        }
        // `check` is the one that judges, which is the whole difference between them.
        Action::Check => {
            if federation.is_partial() {
                for warning in federation.warnings() {
                    let _ = writeln!(err, "error: {}: {}", warning.code.as_str(), warning.message);
                }
                ExitClass::Unavailable
            } else {
                ExitClass::Success
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_implemented_actions_are_read() {
        for name in Action::IMPLEMENTED {
            assert!(Action::from_text(name).is_some(), "{name}");
        }
        for name in Action::DEFERRED {
            assert!(
                Action::from_text(name).is_none(),
                "{name} is not implemented, so it must not parse as one that is"
            );
        }
        assert!(Action::from_text("frobnicate").is_none());
    }
}
