//! `nostdb build`.
//!
//! Analyzes the project's source and commits what it found. The Engine does all of it;
//! this reports the result and chooses an exit class.
//!
//! Structural extraction spends no external AI tokens, so this command needs no budget
//! check and cannot be refused by one. `nostdb plan` is where the cost of the *optional*
//! enrichment that follows is shown, and that is a separate step this build does not run.
//!
//! A tree where every file matches the digest already recorded is not read at all. Anything
//! less than that — one changed file, one deleted file — enters every file into the build,
//! because resolving against a mixture of fresh and previously recorded facts lost edges and
//! the reason is not yet understood.
//!
//! What keeps that affordable is the parse cache: a file whose bytes have not changed is not
//! re-read or re-parsed, but it still enters the build, so the index references resolve
//! against is complete. `--rebuild` distrusts the recorded facts; it does not distrust a
//! parse of bytes that have not changed, because that is not a fact about the database.

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
        "cached_parses": report.cached_parses,
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
        "analyzed   {} files, {} from cache, {} reused, structural {}",
        report.analyzed_files,
        report.cached_parses,
        report.reused_files,
        report.coverage.structural
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
        // Which languages, on both sides.
        //
        // Reported because the note above is true and unactionable on its own. A reader is left
        // to work out whether they excluded their own sources by mistake or whether the language
        // simply has no analyzer yet, and those have opposite fixes: one is a settings change and
        // the other is waiting for a release. A Kotlin repository reporting `0 nodes` was read as
        // a build failure, which is the reasonable reading of a report that says a language was
        // not analyzed without saying which ones are.
        let registry = crate::plan::registry();
        let analyzes = match registry.languages().join(", ") {
            empty if empty.is_empty() => "no language".to_owned(),
            named => named,
        };
        let found: Vec<&str> = report
            .plan
            .languages
            .iter()
            .map(|summary| summary.language.as_str())
            .collect();
        match found.is_empty() {
            // Every file was skipped before classification, so naming what was found would name
            // nothing. The skip reasons below are the answer in that case.
            true => {
                let _ = writeln!(err, "note: it analyzes {analyzes}");
            }
            false => {
                let _ = writeln!(
                    err,
                    "note: it analyzes {analyzes}; this project is {}",
                    found.join(", ")
                );
            }
        }
    }
    for (reason, count) in &report.plan.skipped {
        let _ = writeln!(err, "note: {count} file(s) skipped: {reason}");
    }
    ExitClass::Success
}
