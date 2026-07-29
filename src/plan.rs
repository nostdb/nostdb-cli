//! `nostdb plan`.
//!
//! Reports what a build would do without doing any of it. Root PRD section 17.6 requires
//! a plan before any AI action begins, and this is the command that shows one.
//!
//! Nothing here decides anything. The Engine produces the plan; this renders it and
//! chooses an exit class.

use crate::exit::ExitClass;
use crate::output::Format;
use nostdb_core::analysis::CapabilityRegistry;
use nostdb_core::plan::{BudgetCheck, PlanReport};
use nostdb_core::project::Project;
use nostdb_core::scan::ScanOptions;
use nostdb_core::settings::AiMode;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

fn global_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".nostdb").join("settings.json"))
}

/// The analyzers this build ships.
///
/// Shared with `build`, deliberately. A language `plan` calls unsupported and `build`
/// analyzes would make the two disagree about the same file.
///
/// The Engine owns the list. Falling back to an empty registry rather than failing is
/// deliberate: the only way `builtin_registry` can refuse is if two analyzers declared the
/// same language, which is a defect in this build and not something a user did. Reporting
/// every language as unsupported is then wrong but harmless, where refusing to plan at all
/// would block a command that spends nothing.
pub fn registry() -> CapabilityRegistry {
    nostdb_core::analyze::builtin_registry().unwrap_or_else(|_| CapabilityRegistry::new())
}

fn as_json(report: &PlanReport) -> Value {
    let plan = &report.plan;
    let mut document = json!({
        "plan_version": plan.plan_version,
        // Stated rather than left to be inferred from a zero estimate. A caller deciding
        // whether enrichment may start needs to tell "nothing to do" from "refused", and
        // both produce an estimate of zero.
        "ai_mode": report.ai_mode.as_str(),
        "source_revision": plan.source_revision,
        "scanned_files": plan.scanned_files,
        "structural_files": plan.structural_files,
        "unsupported_files": plan.unsupported_files,
        "semantic_candidates": plan.semantic_candidates,
        "semantic_cache_hits": plan.semantic_cache_hits,
        "estimated_input_tokens": {
            "low": plan.estimated_input_tokens.low,
            "high": plan.estimated_input_tokens.high,
        },
        "estimated_output_tokens": {
            "low": plan.estimated_output_tokens.low,
            "high": plan.estimated_output_tokens.high,
        },
        "budget": {
            "max_input_tokens": plan.budget.max_input_tokens,
            "max_output_tokens": plan.budget.max_output_tokens,
            "max_cost_usd": plan.budget.max_cost_usd,
            "on_exceeded": plan.budget.on_exceeded.as_str(),
        },
        "languages": report.languages.iter().map(|summary| json!({
            "language": summary.language,
            "files": summary.files,
            "bytes": summary.bytes,
            "precision": summary.precision.to_string(),
        })).collect::<Vec<_>>(),
        "skipped": report.skipped.iter().map(|(reason, count)| json!({
            "reason": reason.to_string(),
            "files": count,
        })).collect::<Vec<_>>(),
    });
    if let BudgetCheck::Exceeds {
        field,
        estimated,
        limit,
    } = plan.within_budget()
    {
        document["budget_exceeded"] = json!({
            "field": field,
            "estimated": estimated,
            "limit": limit,
        });
    }
    document
}

fn as_table(report: &PlanReport, out: &mut dyn Write) {
    let plan = &report.plan;
    let _ = writeln!(out, "revision   {}", plan.source_revision);
    let _ = writeln!(
        out,
        "files      {} scanned, {} skipped",
        plan.scanned_files,
        report.skipped_files()
    );
    let _ = writeln!(
        out,
        "structural {} covered, {} unsupported",
        plan.structural_files, plan.unsupported_files
    );

    if !report.languages.is_empty() {
        let _ = writeln!(out);
        for summary in &report.languages {
            let _ = writeln!(
                out,
                "  {:<12} {:>5} files  {:>9} bytes  {}",
                summary.language, summary.files, summary.bytes, summary.precision
            );
        }
    }
    if !report.skipped.is_empty() {
        let _ = writeln!(out);
        for (reason, count) in &report.skipped {
            let _ = writeln!(out, "  {count:>5} {reason}");
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "semantic   {} candidates, {} cached",
        plan.semantic_candidates, plan.semantic_cache_hits
    );
    let _ = writeln!(
        out,
        "tokens     {} in, {} out",
        plan.estimated_input_tokens, plan.estimated_output_tokens
    );
}

/// Runs `nostdb plan [PATH]`.
pub fn run(from: &Path, format: Format, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    let project = match Project::discover(from, global_settings_path().as_deref()) {
        Ok(project) => project,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitClass::for_project_error(&error);
        }
    };
    let analyzers = registry();
    let report = match project.plan(&analyzers, &ScanOptions::default()) {
        Ok(report) => report,
        Err(error) => {
            let _ = writeln!(err, "{error}");
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

    // Everything below is commentary on a plan that was produced successfully, so it goes
    // to stderr and only the last case changes the exit class.
    if analyzers.languages().is_empty() && report.plan.unsupported_files > 0 {
        // Without this the report says "0 covered, 48 unsupported" and leaves a reader to
        // guess whether the project is unusual or the build is. It is the build.
        let _ = writeln!(
            err,
            "note: this build registers no deterministic analyzer, so every language is \
             unsupported"
        );
    }
    if report.ai_mode == AiMode::Off {
        let _ = writeln!(
            err,
            "note: analysis.ai_mode is off, so this run would spend no tokens"
        );
    } else if !report.plan.budget.has_hard_limit() && report.plan.spends_tokens() {
        // Section 17.6: with no configured limit the estimate is shown and the user is
        // asked once. This command only shows; it is the reason the estimate is a band.
        let _ = writeln!(
            err,
            "note: no token limit is configured, so enrichment would ask before starting"
        );
    }

    match report.plan.within_budget() {
        BudgetCheck::Fits => ExitClass::Success,
        // Not a failure of planning — planning is what found it. The class is the one a
        // budget refusal carries, so a pipeline can branch on it before spending anything.
        exceeded => {
            let _ = writeln!(err, "error: {exceeded}");
            ExitClass::AiBudget
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_this_command_uses_is_the_engines() {
        // Not a second list. A language the Engine can analyze and this command calls
        // unsupported would make `plan` disagree with `build` about the same file.
        assert_eq!(
            registry().languages(),
            nostdb_core::analyze::builtin_registry()
                .expect("the built-in registry")
                .languages()
        );
        assert!(registry().languages().contains(&"rust"));
    }

    #[test]
    fn a_registered_language_is_reported_as_deterministic() {
        assert!(
            registry().precision("rust").is_deterministic(),
            "structural extraction of supported source spends no AI tokens, and the plan \
             has to be able to say so"
        );
        // Markdown, deliberately. This used to name Python, and Python gained an analyzer — which made the
        // assertion the opposite of what it was written for while the line above it kept passing. A prose
        // format is the durable choice: no structural analyzer is ever coming for it.
        assert!(!registry().precision("markdown").is_deterministic());
    }
}
