//! `nostdb apply`.
//!
//! Reads a change set from a file and applies it. The Engine decides everything; this reads
//! the file, reports what happened, and chooses an exit class.
//!
//! # Two refusals that are not the same
//!
//! A document that does not satisfy the change-set contract is a validation failure: the
//! file is wrong. A document that is well formed and cannot be applied — a stale baseline,
//! an endpoint that is not there, a linked source a write may not touch — is a different
//! thing, and the contract is explicit that satisfying the document rules is not permission
//! to apply. Keeping them apart is what lets a caller tell "fix the file" from "the database
//! moved".

use crate::exit::ExitClass;
use crate::output::Format;
use nostdb_core::change_document::{code_for, parse};
use nostdb_core::project::Project;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

fn global_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".nostdb").join("settings.json"))
}

/// Runs `nostdb apply FILE`.
pub fn run(
    file: &Path,
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
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) => {
            let _ = writeln!(err, "{}: {error}", file.display());
            return ExitClass::Io;
        }
    };

    let change_set = match parse(&text) {
        Ok(change_set) => change_set,
        Err(errors) => {
            // Every problem, so the caller fixes the file in one pass rather than one
            // failed run per mistake.
            for error in &errors {
                let _ = writeln!(err, "error: {}: {error}", code_for(error).as_str());
            }
            return ExitClass::Validation;
        }
    };

    let report = match project.apply(&change_set) {
        Ok(report) => report,
        Err(error) => {
            // The file was readable and the database refused it. A failed apply preserves
            // the last valid generation, which the Engine guarantees by never writing until
            // the whole set applied to a copy.
            let _ = writeln!(err, "error: {error}");
            return ExitClass::for_project_error(&error);
        }
    };

    let summary = &report.summary;
    match format {
        Format::Json | Format::Jsonl => {
            let document = json!({
                "generation": report.generation.get(),
                "operations": change_set.operations.len(),
                "records": {
                    "nodes_created": summary.nodes_created,
                    "nodes_updated": summary.nodes_updated,
                    "nodes_deleted": summary.nodes_deleted,
                    "edges_created": summary.edges_created,
                    "edges_updated": summary.edges_updated,
                    "edges_deleted": summary.edges_deleted,
                },
                "links": {
                    "upserted": summary.links_upserted,
                    "removed": summary.links_removed,
                },
                "placeholders_resolved": summary.placeholders_resolved,
            });
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(&document).unwrap_or_else(|_| document.to_string())
            );
        }
        Format::Table | Format::Csv => {
            let _ = writeln!(out, "operations {}", change_set.operations.len());
            let _ = writeln!(
                out,
                "nodes      {} created, {} updated, {} deleted",
                summary.nodes_created, summary.nodes_updated, summary.nodes_deleted
            );
            let _ = writeln!(
                out,
                "edges      {} created, {} updated, {} deleted",
                summary.edges_created, summary.edges_updated, summary.edges_deleted
            );
            let _ = writeln!(out, "generation {}", report.generation);
        }
    }
    ExitClass::Success
}
