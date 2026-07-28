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
use nostdb_core::project::Project;
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
  query [CYPHER] [--database @NAME]
                           Run one statement, or open the REPL when none is given
  link ACTION              Declare, remove, or report on links to other graphs
  plan [PATH]              Report what a build would do, without doing any of it
  build [PATH] [--rebuild] Analyze the project's source and commit what it found
  apply FILE               Apply a change set to the active project
  sync [PATH]              Bring .nostdb and .nost into agreement, or say why not
  plugin ACTION            Install a plugin from a pinned GitHub source
  catalog ACTION           Register, remove, or list named databases for this user
  server [ACTION]          Run the per-user local daemon, or report on it
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
        "link" => {
            "\
nostdb link list [--format FORMAT] [--project PATH]
nostdb link check [--format FORMAT] [--project PATH]
nostdb link add SOURCE [as ALIAS] [--format FORMAT] [--project PATH]
nostdb link remove SOURCE [--format FORMAT] [--project PATH]

  list      reports every declared link and what became of it
  check     reports the same, and fails when any link is unreachable
  add       declares a link and mirrors it into the settings
  remove    removes the declaration and its settings entry

SOURCE is the canonical locator, and it is the link's identity: moving a target
means relinking it. The alias is written the way the language writes it, `as
NAME`, and it is optional. It is stored in the graph and never in the settings,
because an alias in a machine-local file would make one link mean two different
things on two checkouts.

`add` does not require the target to be reachable. Whether a source resolves is
a separate question from whether it is declared, and `check` is the command that
asks it.

`remove` removes a declaration, never data. Nothing reached through a link was
ever part of this database.

  refresh   resolves every remote link and records the commit it now points at

`refresh` is the only thing that advances a snapshot. A query never does, so two
queries a week apart see the same commit unless somebody asked for a newer one.

A local link is read live at every query and has no snapshot to advance, which
`refresh` reports rather than treating as a failure. A link that cannot be
reached keeps the commit it had: forgetting where it pointed would turn one
unreachable minute into a rebuild of everything it reached.

The provider executable is named by NOSTDB_GITHUB_PROVIDER, and is started only
when a remote link needs one.
"
        }
        "query" => {
            "\
nostdb query [CYPHER] [--format FORMAT] [--project PATH] [--database @NAME]

With a statement, runs it and reports the result. With none, opens the REPL,
where a statement may span lines and ends at a `;`, and `:help` lists the
commands the session itself understands.

The subset is openCypher, and it is explicit: unsupported syntax is reported
with its source range and never run under a guessed alternative, because a query
that quietly meant something else is worse than one that refused.

Result order is undefined without ORDER BY, and no output format implies
otherwise.

  --format table   the default, for a person
  --format json    one document, data on stdout and diagnostics on stderr
  --format jsonl   one row per line
  --format csv     the same rows, with a header

  --database @NAME runs through the per-user daemon against a named database

A path-based query needs no daemon. `--database @NAME` is the only form that
does, because a name is what the daemon's catalog resolves and a path never
needed resolving.

A query sees its root database and everything reachable through its declared
links, and writes affect only the root. Linked data is read-only from the root
transaction.
"
        }
        "sync" => {
            "\
nostdb sync [PATH]

Brings .nostdb and .nost into agreement, in whichever direction changed.

Synchronization compares database generations and content digests, never
timestamps: `newest wins` decides by a clock that two machines do not share.

If both representations changed from the same baseline, neither is modified and
SYNC_CONFLICT is reported. There is no option to prefer one, because preferring
either would discard the other's changes and nothing here can know which of the
two a person meant to keep.

`nost: false` removes only the generated source it was configured to write. It
never removes the database or a file this build did not create.
"
        }
        "plugin" => {
            "\
nostdb plugin add SOURCE [--scope project|global] [--project PATH]

  add       fetches a plugin, checks it, and records what was approved

SOURCE is a GitHub source:

  https://github.com/OWNER/REPOSITORY[?ref=GIT-REF][#SUBDIRECTORY]

The ref is required. It is resolved once to an immutable commit, and everything
after that uses the commit, so a plugin does not change underneath a project
that installed it.

Installation never executes plugin code. It resolves the ref, enumerates the
tree, refuses a path that escapes or a tree over a fixed limit, reads and
validates the manifest, checks the manifest's Engine range against this build,
and only then writes anything. Running a plugin is a separate act.

Two digests are recorded. The manifest digest detects an edited request; the
tree digest detects edited code behind an unchanged request, which is the more
dangerous of the two because the plugin's stated intent would look unchanged.

Reinstalling the same commit with different bytes is refused rather than
written over: a commit is immutable, so different bytes mean something between
the host and this machine is not what it was. No option installs over that.

  --scope project    installs into the project, and takes precedence over global
  --scope global     installs into ~/.nostdb/plugins for every project

With no scope, an interactive session in a project is asked and project is
recommended; a non-interactive one takes project; outside a project it is
global.

A plugin is not sandboxed. It runs as your user, with your files, and the
process boundary is the whole of the isolation.

The provider executable is named by NOSTDB_GITHUB_PROVIDER. A plugin source
always needs one, because nothing in this command surface reaches GitHub itself.
"
        }
        "apply" => {
            "\
nostdb apply FILE [--format FORMAT] [--project PATH]

Reads a change set from FILE and applies it. An analyzer builds one from source,
an AI Skill proposes one, and a person may write one by hand.

Two refusals that are not the same thing. A document that does not satisfy the
change-set contract exits 3: the file is wrong, and every problem is reported so
it can be fixed in one pass. A document that is well formed and cannot be applied
— a stale baseline, an endpoint that is not there, an endpoint in a linked source
a write may not touch — is a different failure, because satisfying the document
rules is not permission to apply.

A change set states the generation it was computed against, and one computed
against a different generation is refused. It resolved identifiers against a graph
it read, and applying it to a graph that has moved would overwrite work nobody saw.

A failed apply preserves the last valid generation.
"
        }
        "build" => {
            "\
nostdb build [PATH] [--rebuild] [--format FORMAT] [--project PATH]

Analyzes the project's source and commits the structural facts it found: files,
the items they declare, what contains what, and which calls resolve.

Structural extraction spends no external AI tokens, so this cannot be refused by
a budget. Optional AI enrichment is a separate step; `nostdb plan` is where its
cost is shown.

A tree where every file matches the digest already recorded is not read at all,
and commits nothing. Anything less than that enters every file into the build:
resolving references against a mixture of freshly read and previously recorded
facts turned out to lose edges, and until that is understood a build that reads
one file considers them all.

A parse is cached, so a file whose bytes have not changed is not re-read or
re-parsed even then. `--rebuild` distrusts the recorded facts; it does not
distrust a parse of unchanged bytes, which is not a fact about the database.

A rebuild replaces only this analyzer's own contributions for the files it read.
Anything a person contributed to the same record survives, and a record the
source no longer declares is removed.

A record keeps its identifier across a rebuild as long as its qualified name is
unchanged, so moving a function down a file costs nothing. Renaming one retires
the old record and creates a new one, because a renamed function is not the same
function to anything that referred to it by name.

A call whose name matches nothing in the project is counted as unresolved rather
than given a placeholder record. This build reads syntax without resolving names,
so it cannot tell a missing symbol from one in a dependency it never saw.

A failed build preserves the last valid generation.
"
        }
        "plan" => {
            "\
nostdb plan [PATH] [--format FORMAT] [--project PATH]

Reports what a build would do, and does none of it. Walks the project's tree,
names the language of every file, and reports which an analyzer covers, which are
unsupported, and what AI enrichment would cost.

No AI action begins before a plan exists. That is why this is a command rather
than build output: the estimate is what a budget check runs against, and it is
shown before anything is spent.

Every file the scan reaches is accounted for. A file that is not analyzed appears
under the reason it was excluded — ignored, sensitive, unclassified, too large,
binary, a symlink that was not followed, or permission denied.

Token counts are a band rather than a number, because they are estimated from
byte counts rather than tokenized. A budget check compares the top of the band, so
a run that could exceed a hard limit never starts.

Exits 8 when the estimated run would cross a configured token limit. Planning
succeeded in that case; it is the plan that says the build cannot proceed.
"
        }
        "catalog" => {
            "\
nostdb catalog add NAME PATH
nostdb catalog remove NAME
nostdb catalog list [--format FORMAT]

Maps stable local names to databases on this machine, for this operating-system
user, in ~/.nostdb/catalog.json. A name is what `--database @name` resolves.

A relative PATH is resolved against the working directory, because the catalog is
read from wherever a later command happens to run and a relative path there has no
anchor.

Registering a name does not open the database. A name may point at a disk that is
not currently mounted, and the failure is reported by the command that tries to use
it rather than by the catalog.

Needs no running daemon. Registering a name is what someone does before starting
one.
"
        }
        "server" => {
            "\
nostdb server [start|status|stop|run]

Manages the one daemon this operating-system user may run. `nostdb server` is an
alias for `start`.

  run       Stay in the foreground, for a service manager or for debugging
  status    Report whether a daemon is running

status asks the operating-system lock rather than the socket. A killed daemon
leaves its socket file behind, so a socket proves nothing.

The daemon manages named databases. A path-based command never needs it, and
starting one does not change what any path-based command does.
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
            ExitClass::for_project_error(&error)
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
            return ExitClass::for_project_error(&error);
        }
    };

    let graph = match project.read_graph() {
        Ok(graph) => graph,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitClass::for_project_error(&error);
        }
    };

    // An orphan entry is a warning: the settings file names a link the graph no longer
    // declares, and refusing to export over that would be worse than saying so.
    let orphans = project.orphan_link_settings(&graph);
    let _ = report(&orphans, err);

    // The Engine writes the file and records the baseline in one step. Writing the file
    // here and the baseline separately would leave a window where the two disagree about
    // whether they agree.
    let target = project.nost_path();
    if let Err(error) = project.export_nost() {
        let _ = writeln!(err, "{error}");
        return ExitClass::for_project_error(&error);
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

    /// Every command the parser accepts.
    ///
    /// Stated once and used by both checks below. Each of them used to carry its own list of
    /// six, written when the surface had six commands and never extended: a test named
    /// `every_command` that covered under half of them read as coverage it did not have.
    const EVERY_COMMAND: [&str; 14] = [
        "help", "init", "check", "convert", "export", "query", "link", "plan", "build", "apply",
        "sync", "plugin", "catalog", "server",
    ];

    #[test]
    fn the_summary_lists_every_command_the_parser_accepts() {
        let (text, _, class) = rendered(None);
        assert_eq!(class, ExitClass::Success);
        for command in EVERY_COMMAND.iter().chain(["--version"].iter()) {
            assert!(text.contains(command), "the summary omits {command}");
        }
    }

    #[test]
    fn every_command_has_a_help_topic() {
        // A command the summary advertises and help cannot describe is a gap a reader
        // finds before a maintainer does.
        for command in EVERY_COMMAND.iter().chain(["version"].iter()) {
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
