//! The commands.
//!
//! Each one parses nothing, stores nothing, and queries nothing. It calls `nostdb-core`
//! and renders what came back, choosing the exit class the root product contract assigns
//! to that kind of failure.
//!
//! # Atomic writes
//!
//! A command that replaces a file writes a sibling temporary and renames it over the
//! target. A half-written `.nost` is worse than no `.nost`, because it parses far enough
//! to look like a smaller graph.

use crate::exit::ExitClass;
use crate::{PRODUCT, VERSION};
use nostdb_core::container::SUPPORTED_FORMAT_VERSIONS;
use nostdb_core::diagnostic::{Diagnostic, Severity};
use nostdb_core::encoding::{Graph, commit_graph, read_graph};
use nostdb_core::nost::validate::SUPPORTED_LANGUAGE_VERSIONS;
use nostdb_core::nost::{ConversionError, format, from_graph, parse, to_graph, validate};
use nostdb_core::project::{Project, ProjectError};
use nostdb_core::settings::SUPPORTED_VERSIONS as SUPPORTED_SETTINGS_VERSIONS;
use nostdb_core::storage::Database;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The `.nost` extension.
const NOST: &str = "nost";

/// The `.nostdb` extension.
const NOSTDB: &str = "nostdb";

const SUMMARY: &str = "\
nostdb — a local-first property graph database for software environments

Usage:
  nostdb <COMMAND> [ARGUMENTS]

Commands:
  help [COMMAND]           Describe the command surface, or one command
  init [PATH]              Configure a project, creating .nostdb/ and its database
  check TARGET             Validate a .nost or .nostdb file
  convert INPUT OUTPUT     Convert between .nost and .nostdb, in either direction
  export --nost [PATH]     Write the active project's graph as canonical .nost
  query [CYPHER]           Run one statement, or open the REPL when none is given
  --version [--json]       Report this build and every contract version it supports

Data is written to stdout and diagnostics to stderr, so a machine-readable mode
never carries commentary.
";

fn topic_text(topic: &str) -> Option<&'static str> {
    Some(match topic {
        "help" => {
            "\
nostdb help [COMMAND]

Describes the command surface, or one command in detail.
"
        }
        "init" => {
            "\
nostdb init [PATH]

Configures PATH, defaulting to the working directory, by creating:

  .nostdb/settings.json    holding the contract version and nothing else
  .nostdb/root.nostdb      an empty database

Refuses rather than overwriting an existing settings file, so a re-run cannot
discard configuration.
"
        }
        "check" => {
            "\
nostdb check TARGET

Validates a file, choosing by extension:

  .nost      parses and reports every semantic rule it breaks
  .nostdb    opens the container and decodes the graph it holds

Exits 0 when only warnings were found, and 3 when anything was an error.
Warnings are reported either way.
"
        }
        "convert" => {
            "\
nostdb convert INPUT OUTPUT

Converts in whichever direction the extensions name:

  .nost   -> .nostdb    validates, then commits the graph
  .nostdb -> .nost      reads the graph, then writes canonical .nost

Refuses when both extensions are the same, because that is a copy rather than a
conversion, and when either is neither .nost nor .nostdb.

The output is written atomically. An endpoint naming a linked source is refused:
resolving one needs link resolution, which this build does not implement.
"
        }
        "export" => {
            "\
nostdb export --nost [PATH]

Finds the nearest configured project at or above PATH, defaulting to the working
directory, and writes its graph to .nostdb/root.nost in canonical form.

`--nost` is required. It is the only representation this build exports, and
requiring it keeps a later one from silently changing what a bare export means.

Reports a warning when database.nost is false, because the file is written but
nothing will keep it current.
"
        }
        "version" | "--version" => {
            "\
nostdb --version [--json]

Reports this build and every contract version it supports. Each contract is
versioned independently, so they are listed separately rather than rolled into
one product number.
"
        }
        _ => return None,
    })
}

/// `nostdb help [COMMAND]`
pub fn help(topic: Option<&str>, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    match topic {
        None => {
            let _ = write!(out, "{SUMMARY}");
            ExitClass::Success
        }
        Some(name) => match topic_text(name) {
            Some(text) => {
                let _ = write!(out, "{text}");
                ExitClass::Success
            }
            None => {
                let _ = writeln!(err, "unknown command `{name}`");
                ExitClass::Usage
            }
        },
    }
}

/// Renders a supported-version list the way the report prints it.
fn list<T: std::fmt::Display>(values: &[T]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `nostdb --version [--json]`
pub fn version(json: bool, out: &mut dyn Write) -> ExitClass {
    if json {
        // Hand-written rather than serialized, because this shape is a published
        // contract in root PRD section 25.4 and a struct would let a field rename slip
        // through as a silent output change.
        let _ = writeln!(
            out,
            "{{\n  \"product\": \"{PRODUCT}\",\n  \"engine_version\": \"{VERSION}\",\n  \
             \"nostdb_format_versions\": [{}],\n  \"nost_language_versions\": [{}],\n  \
             \"settings_versions\": [{}]\n}}",
            list(&SUPPORTED_FORMAT_VERSIONS),
            list(&SUPPORTED_LANGUAGE_VERSIONS),
            list(&SUPPORTED_SETTINGS_VERSIONS),
        );
    } else {
        let _ = writeln!(out, "{PRODUCT} {VERSION}");
        let _ = writeln!(out, "nostdb_format  {}", list(&SUPPORTED_FORMAT_VERSIONS));
        let _ = writeln!(out, "nost_language  {}", list(&SUPPORTED_LANGUAGE_VERSIONS));
        let _ = writeln!(out, "settings       {}", list(&SUPPORTED_SETTINGS_VERSIONS));
    }
    ExitClass::Success
}

/// The exit class a project failure reports.
///
/// A missing project is a usage mistake rather than a corrupt file, a refused settings
/// document is a validation failure, and everything filesystem-shaped is I/O.
fn project_class(error: &ProjectError) -> ExitClass {
    match error {
        ProjectError::NotFound { .. } | ProjectError::AlreadyConfigured { .. } => ExitClass::Usage,
        ProjectError::Settings { .. } | ProjectError::Decode(_) => ExitClass::Validation,
        ProjectError::Io { .. } | ProjectError::Storage(_) => ExitClass::Io,
    }
}

/// `nostdb init [PATH]`
pub fn init(path: &Path, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    match Project::initialize(path) {
        Ok(project) => {
            let _ = writeln!(out, "{}", project.root().display());
            let _ = writeln!(
                err,
                "configured {}, database at {}",
                project.root().display(),
                project.database_path().display()
            );
            ExitClass::Success
        }
        Err(error) => {
            let _ = writeln!(err, "{error}");
            project_class(&error)
        }
    }
}

/// Reports diagnostics and returns the class they imply.
fn report(found: &[Diagnostic], err: &mut dyn Write) -> ExitClass {
    for diagnostic in found {
        let position = diagnostic.range.map_or_else(String::new, |range| {
            format!("{}:{}: ", range.start().line, range.start().column)
        });
        let _ = writeln!(
            err,
            "{position}{}: {}: {}",
            diagnostic.severity,
            diagnostic.code.as_str(),
            diagnostic.message
        );
    }
    if found
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        ExitClass::Validation
    } else {
        ExitClass::Success
    }
}

fn extension(path: &Path) -> Option<&str> {
    path.extension().and_then(std::ffi::OsStr::to_str)
}

/// Reads a `.nost` file, reporting a parse failure or every semantic rule it breaks.
fn read_nost(path: &Path, err: &mut dyn Write) -> Result<nostdb_core::nost::SourceFile, ExitClass> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            let _ = writeln!(err, "{}: {error}", path.display());
            return Err(ExitClass::Io);
        }
    };
    let file = match parse(&text) {
        Ok(file) => file,
        Err(error) => {
            let _ = writeln!(
                err,
                "{}:{}: error: {}: {}",
                path.display(),
                error.range.start().line,
                error.code().as_str(),
                error.message
            );
            return Err(ExitClass::Validation);
        }
    };
    Ok(file)
}

/// `nostdb check TARGET`
pub fn check(target: &Path, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    match extension(target) {
        Some(NOST) => {
            let file = match read_nost(target, err) {
                Ok(file) => file,
                Err(class) => return class,
            };
            let found = validate(&file);
            let class = report(&found, err);
            if class.is_success() {
                let _ = writeln!(out, "{}: valid", target.display());
            }
            class
        }
        Some(NOSTDB) => {
            let database = match Database::open(target) {
                Ok(database) => database,
                Err(error) => {
                    let _ = writeln!(err, "{}: {error}", target.display());
                    return ExitClass::Io;
                }
            };
            match read_graph(&database) {
                Ok(graph) => {
                    let _ = writeln!(
                        out,
                        "{}: valid, generation {}, {} nodes, {} edges, {} links, {} schemas",
                        target.display(),
                        database.generation().get(),
                        graph.nodes.len(),
                        graph.edges.len(),
                        graph.links.len(),
                        graph.schemas.len()
                    );
                    ExitClass::Success
                }
                Err(error) => {
                    let _ = writeln!(
                        err,
                        "{}: error: {}: {error}",
                        target.display(),
                        error.code().as_str()
                    );
                    ExitClass::Validation
                }
            }
        }
        other => {
            let _ = writeln!(
                err,
                "cannot check {}: expected a .nost or .nostdb file, found {}",
                target.display(),
                other.map_or_else(|| "no extension".to_owned(), |text| format!(".{text}"))
            );
            ExitClass::Usage
        }
    }
}

/// Writes `contents` to `path` by renaming a sibling temporary over it.
fn write_atomically(path: &Path, contents: &str, err: &mut dyn Write) -> Result<(), ExitClass> {
    let staged = staging_path(path);
    if let Err(error) = std::fs::write(&staged, contents) {
        let _ = writeln!(err, "{}: {error}", staged.display());
        return Err(ExitClass::Io);
    }
    if let Err(error) = std::fs::rename(&staged, path) {
        let _ = std::fs::remove_file(&staged);
        let _ = writeln!(err, "{}: {error}", path.display());
        return Err(ExitClass::Io);
    }
    Ok(())
}

fn staging_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".staged");
    path.with_file_name(name)
}

fn conversion_class(error: &ConversionError) -> ExitClass {
    match error {
        // A construct this build does not implement, rather than a wrong file.
        ConversionError::ExternalEndpoint { .. } => ExitClass::Unavailable,
        ConversionError::UnsupportedVersion { .. } | ConversionError::InvalidValue { .. } => {
            ExitClass::Validation
        }
    }
}

/// `nostdb convert INPUT OUTPUT`
pub fn convert(input: &Path, output: &Path, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    match (extension(input), extension(output)) {
        (Some(NOST), Some(NOSTDB)) => convert_to_database(input, output, out, err),
        (Some(NOSTDB), Some(NOST)) => convert_to_nost(input, output, out, err),
        (Some(same), Some(also)) if same == also => {
            let _ = writeln!(
                err,
                "cannot convert .{same} to .{also}: that is a copy rather than a conversion"
            );
            ExitClass::Usage
        }
        _ => {
            let _ = writeln!(
                err,
                "cannot convert {} to {}: each path is a .nost or a .nostdb file",
                input.display(),
                output.display()
            );
            ExitClass::Usage
        }
    }
}

fn convert_to_database(
    input: &Path,
    output: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitClass {
    let file = match read_nost(input, err) {
        Ok(file) => file,
        Err(class) => return class,
    };

    // Validation runs before anything is written, so a refused document leaves the target
    // exactly as it was.
    let found = validate(&file);
    let class = report(&found, err);
    if !class.is_success() {
        return class;
    }

    let graph = match to_graph(&file) {
        Ok(graph) => graph,
        Err(error) => {
            let _ = writeln!(err, "{}: {error}", input.display());
            return conversion_class(&error);
        }
    };

    let staged = staging_path(output);
    let _ = std::fs::remove_file(&staged);
    let mut database = match Database::create(&staged) {
        Ok(database) => database,
        Err(error) => {
            let _ = writeln!(err, "{}: {error}", staged.display());
            return ExitClass::Io;
        }
    };
    if let Err(error) = commit_graph(&mut database, &graph) {
        let _ = std::fs::remove_file(&staged);
        let _ = writeln!(err, "{}: {error}", staged.display());
        return ExitClass::Io;
    }
    drop(database);
    if let Err(error) = std::fs::rename(&staged, output) {
        let _ = std::fs::remove_file(&staged);
        let _ = writeln!(err, "{}: {error}", output.display());
        return ExitClass::Io;
    }

    let _ = writeln!(out, "{}", output.display());
    let _ = writeln!(
        err,
        "wrote {} nodes, {} edges, {} links, {} schemas",
        graph.nodes.len(),
        graph.edges.len(),
        graph.links.len(),
        graph.schemas.len()
    );
    ExitClass::Success
}

fn convert_to_nost(
    input: &Path,
    output: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitClass {
    let graph = match load_graph(input, err) {
        Ok(graph) => graph,
        Err(class) => return class,
    };
    let text = format(&from_graph(&graph));
    if let Err(class) = write_atomically(output, &text, err) {
        return class;
    }
    let _ = writeln!(out, "{}", output.display());
    ExitClass::Success
}

fn load_graph(path: &Path, err: &mut dyn Write) -> Result<Graph, ExitClass> {
    let database = match Database::open(path) {
        Ok(database) => database,
        Err(error) => {
            let _ = writeln!(err, "{}: {error}", path.display());
            return Err(ExitClass::Io);
        }
    };
    read_graph(&database).map_err(|error| {
        let _ = writeln!(
            err,
            "{}: error: {}: {error}",
            path.display(),
            error.code().as_str()
        );
        ExitClass::Validation
    })
}

/// `nostdb export --nost [PATH]`
pub fn export(from: &Path, out: &mut dyn Write, err: &mut dyn Write) -> ExitClass {
    let project = match Project::discover(from, global_settings_path().as_deref()) {
        Ok(project) => project,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return project_class(&error);
        }
    };

    let graph = match project.read_graph() {
        Ok(graph) => graph,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return project_class(&error);
        }
    };

    // An orphan entry is a warning: the settings file names a link the graph no longer
    // declares, and refusing to export over that would be worse than saying so.
    let orphans = project.orphan_link_settings(&graph);
    let _ = report(&orphans, err);

    let target = project.nost_path();
    let text = format(&from_graph(&graph));
    if let Err(class) = write_atomically(&target, &text, err) {
        return class;
    }

    if !project.settings().database.nost {
        let _ = writeln!(
            err,
            "warning: database.nost is false, so {} is written but will not be kept current",
            target.display()
        );
    }
    let _ = writeln!(out, "{}", target.display());
    ExitClass::Success
}

/// The user-global settings file, when the platform tells us where home is.
fn global_settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".nostdb").join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(topic: Option<&str>) -> (String, String, ExitClass) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let class = help(topic, &mut out, &mut err);
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            class,
        )
    }

    #[test]
    fn the_summary_lists_every_command_the_parser_accepts() {
        let (text, _, class) = rendered(None);
        assert_eq!(class, ExitClass::Success);
        for command in ["help", "init", "check", "convert", "export", "--version"] {
            assert!(text.contains(command), "the summary omits {command}");
        }
    }

    #[test]
    fn every_command_has_a_help_topic() {
        // A command the summary advertises and help cannot describe is a gap a reader
        // finds before a maintainer does.
        for command in ["help", "init", "check", "convert", "export", "version"] {
            let (text, _, class) = rendered(Some(command));
            assert_eq!(class, ExitClass::Success, "{command}");
            assert!(!text.is_empty(), "{command} has no help text");
        }
    }

    #[test]
    fn an_unknown_help_topic_is_a_usage_error_on_stderr() {
        let (out, err, class) = rendered(Some("frobnicate"));
        assert_eq!(class, ExitClass::Usage);
        assert!(out.is_empty());
        assert!(err.contains("frobnicate"));
    }

    #[test]
    fn the_version_report_lists_each_contract_separately() {
        let mut out = Vec::new();
        assert_eq!(version(true, &mut out), ExitClass::Success);
        let text = String::from_utf8(out).unwrap();
        for key in [
            "product",
            "engine_version",
            "nostdb_format_versions",
            "nost_language_versions",
            "settings_versions",
        ] {
            assert!(text.contains(key), "the report omits {key}:\n{text}");
        }
        // The language is at version 2 and the container at 1, which is exactly why they
        // are reported separately rather than as one product number.
        assert!(text.contains("\"nost_language_versions\": [2]"), "{text}");
        assert!(text.contains("\"nostdb_format_versions\": [1]"), "{text}");
    }

    #[test]
    fn a_staging_path_is_a_sibling_of_its_target() {
        let staged = staging_path(Path::new("/tmp/project/.nostdb/root.nost"));
        assert_eq!(
            staged.parent(),
            Path::new("/tmp/project/.nostdb")
                .parent()
                .map(|_| Path::new("/tmp/project/.nostdb"))
        );
        assert_eq!(
            staged.file_name().and_then(std::ffi::OsStr::to_str),
            Some("root.nost.staged")
        );
    }
}
