//! `nostdb sync`.
//!
//! The decision is the Engine's; this reports it and chooses an exit class.

use crate::exit::ExitClass;
use nostdb_core::project::{Project, SyncAction};
use std::io::Write;
use std::path::{Path, PathBuf};

fn global_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".nostdb").join("settings.json"))
}

/// Runs `nostdb sync [PATH]`.
pub fn run(from: &Path, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    let project = match Project::discover(from, global_settings_path().as_deref()) {
        Ok(project) => project,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitClass::for_project_error(&error);
        }
    };

    let report = match project.synchronize() {
        Ok(report) => report,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitClass::for_project_error(&error);
        }
    };

    for diagnostic in &report.diagnostics {
        let _ = writeln!(
            err,
            "{}: {}: {}",
            diagnostic.severity,
            diagnostic.code.as_str(),
            diagnostic.message
        );
    }

    match report.action {
        SyncAction::UpToDate => {
            let _ = writeln!(out, "up to date");
            ExitClass::Success
        }
        SyncAction::Adopted { generation } => {
            let _ = writeln!(
                out,
                "adopted {} at generation {}",
                project.nost_path().display(),
                generation.get()
            );
            ExitClass::Success
        }
        SyncAction::Materialized => {
            let _ = writeln!(out, "{}", project.nost_path().display());
            let _ = writeln!(err, "materialized from the database");
            ExitClass::Success
        }
        SyncAction::NotMaterialized => {
            let _ = writeln!(out, "up to date");
            let _ = writeln!(
                err,
                "database.nost is false and no {} exists, so there is nothing to compare",
                project.nost_path().display()
            );
            ExitClass::Success
        }
        SyncAction::NoBaseline => {
            let _ = writeln!(
                err,
                "error: nothing records what the two representations last agreed on, so \
                 neither can be called the one that changed"
            );
            let _ = writeln!(
                err,
                "  run `nostdb export --nost` to make the database authoritative, or \
                 `nostdb convert {} {}` to adopt the document",
                project.nost_path().display(),
                project.database_path().display()
            );
            ExitClass::Conflict
        }
        // Both refusals are exit class 4: synchronization could not proceed, and the
        // product contract gives that class to a sync conflict.
        SyncAction::NostStale => {
            let _ = writeln!(
                err,
                "  regenerate it with `nostdb export --nost` once you have kept whatever \
                 the file still holds"
            );
            ExitClass::Conflict
        }
        SyncAction::Conflict => {
            let _ = writeln!(
                err,
                "  resolving this is a human decision: both sides hold work derived from \
                 one baseline, so preferring either would discard the other"
            );
            ExitClass::Conflict
        }
    }
}
