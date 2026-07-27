//! `nostdb build`.
//!
//! Analyzes the project's source and commits what it found. The Engine does all of it;
//! this reports the result and chooses an exit class.
//!
//! Structural extraction spends no external AI tokens, so this command needs no budget
//! check and cannot be refused by one. `nostdb plan` is where the cost of the *optional*
//! enrichment that follows is shown, and that is a separate step this build does not run.
//!
//! A tree where every file matches the digest already recorded is not read at all.
//! Anything less than that — one changed file, one deleted file — reads everything, because
//! resolving references against a mixture of fresh and reused records lost edges and the
//! reason is not yet understood. `--rebuild` reads everything unconditionally.

use crate::exit::ExitClass;
use crate::output::Format;
use crate::plan::registry;
use nostdb_core::project::{BuildReport, Project};
use nostdb_core::scan::ScanOptions;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

fn global_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".nostdb").join("settings.json"))
}

fn as_json(report: &BuildReport) -> Value {
    let summary = &report.summary;
    json!({
        "generation": report.generation.get(),
        "source_revision": report.revision,
        "analyzed_files": report.analyzed_files,
        "reused_files": report.reused_files,
        "records": {
            "nodes_created": summary.nodes_created,
            "nodes_updated": summary.nodes_updated,
            "nodes_deleted": summary.nodes_deleted,
            "edges_created": summary.edges_created,
            "edges_updated": summary.edges_updated,
            "edges_deleted": summary.edges_deleted,
        },
        "references": {
            "resolved": report.resolved_references,
            "unresolved": report.coverage.unresolved_units,
        },
        "coverage": {
            "coverage_version": report.coverage.coverage_version,
            "structural": report.coverage.structural.to_string(),
            "semantic": report.coverage.semantic.to_string(),
            "skipped_sources": report.coverage.skipped_sources.len(),
        },
    })
}

fn as_table(report: &BuildReport, out: &mut dyn Write) {
    let summary = &report.summary;
    let _ = writeln!(out, "revision   {}", report.revision);
    let _ = writeln!(
        out,
        "analyzed   {} files, {} reused, structural {}",
        report.analyzed_files, report.reused_files, report.coverage.structural
    );
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
    let _ = writeln!(
        out,
        "references {} resolved, {} unresolved",
        report.resolved_references, report.coverage.unresolved_units
    );
    let _ = writeln!(out, "generation {}", report.generation);
}

/// Runs `nostdb build [PATH]`.
pub fn run(
    from: &Path,
    format: Format,
    rebuild: bool,
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
    let report = match project.build(&registry(), &ScanOptions::default(), rebuild) {
        Ok(report) => report,
        Err(error) => {
            // A failed build preserves the last valid generation, which the Engine
            // guarantees by never writing until the whole change set applied.
            let _ = writeln!(err, "error: {error}");
            return ExitClass::for_project_error(&error);
        }
    };

    match format {
        Format::Json | Format::Jsonl => {
            let document = as_json(&report);
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(&document).unwrap_or_else(|_| document.to_string())
            );
        }
        Format::Table | Format::Csv => as_table(&report, out),
    }

    if report.analyzed_files == 0 && report.reused_files > 0 {
        let _ = writeln!(
            err,
            "note: every file matched the digest already recorded, so nothing was rebuilt"
        );
    } else if report.analyzed_files == 0 {
        // Not a failure. A project with nothing this build reads is a fact about the
        // project, and exiting non-zero over it would break a pipeline that runs `build`
        // before knowing what a repository contains.
        let _ = writeln!(
            err,
            "note: no file has a language this build analyzes, so nothing was committed"
        );
    }
    for (reason, count) in &report.plan.skipped {
        let _ = writeln!(err, "note: {count} file(s) skipped: {reason}");
    }
    ExitClass::Success
}
