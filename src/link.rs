//! `nostdb link list`, `check`, `add`, `remove`, and `refresh`.
//!
//! `list` and `check` report. `add` and `remove` change the declaration in the database
//! and its mirror in the settings, which the Engine does through the multi-file journal;
//! nothing here writes either file.
//!
//! `refresh` advances a remote snapshot to a newer immutable commit. It was refused until the
//! GitHub provider existed, because a local link has no snapshot to advance — it is read live at
//! every query — and implementing it against a local source would have meant inventing a meaning
//! the product contract does not give it.
//!
//! It now resolves through a provider process, which this module starts and stops. A project whose
//! links are all local still launches nothing: a local link has nothing to refresh, so needing a
//! provider installed to be told so would be the same invention in a new place.

use crate::exit::ExitClass;
use crate::output::Format;
use nostdb_core::federation::{Federation, LinkStatus};
use nostdb_core::project::{LinkChange, Project, RefreshOutcome, RefreshedSnapshot};
use nostdb_core::provider::ProviderClient;
use nostdb_core::provider_process::ProviderProcess;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

/// What `link` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Report every declared link and what became of it.
    List,
    /// Report the same, and fail when any link is unreachable.
    Check,
    /// Declare a link.
    Add {
        /// The canonical locator, which is the link's identity.
        source: String,
        /// The optional alias, which lives in the graph and never in settings.
        alias: Option<String>,
    },
    /// Remove a declaration.
    Remove {
        /// The locator naming the link to remove.
        source: String,
    },
    /// Resolve every remote link and record the commit it now points at.
    Refresh,
}

impl Action {
    /// Subcommands this build implements.
    pub const IMPLEMENTED: [&'static str; 5] = ["list", "check", "add", "remove", "refresh"];

    /// Subcommands the product contract names and this build does not implement.
    pub const DEFERRED: [&'static str; 0] = [];

    /// Reports whether this action changes state.
    #[must_use]
    pub const fn writes(&self) -> bool {
        matches!(self, Self::Add { .. } | Self::Remove { .. } | Self::Refresh)
    }
}

/// Reads an action and its operands, returning what is left for the shared options.
///
/// The alias is written `as NAME`, matching the `.nost` declaration rather than inventing
/// a second spelling for the same thing. It is optional there and optional here.
///
/// # Errors
///
/// Returns a message naming what was missing or unexpected.
pub fn parse<'a>(words: &'a [&'a str]) -> Result<(Action, &'a [&'a str]), String> {
    let Some((first, rest)) = words.split_first() else {
        return Err(format!("`link` needs an action: {:?}", Action::IMPLEMENTED));
    };
    match *first {
        "list" => Ok((Action::List, rest)),
        "check" => Ok((Action::Check, rest)),
        "add" => {
            let Some((source, rest)) = rest.split_first() else {
                return Err("`link add` needs a source: `link add SOURCE [as ALIAS]`".to_owned());
            };
            match rest.split_first() {
                Some((&"as", after)) => {
                    let Some((alias, after)) = after.split_first() else {
                        return Err("`as` needs an alias".to_owned());
                    };
                    Ok((
                        Action::Add {
                            source: (*source).to_owned(),
                            alias: Some((*alias).to_owned()),
                        },
                        after,
                    ))
                }
                _ => Ok((
                    Action::Add {
                        source: (*source).to_owned(),
                        alias: None,
                    },
                    rest,
                )),
            }
        }
        "remove" => {
            let Some((source, rest)) = rest.split_first() else {
                return Err("`link remove` needs a source: `link remove SOURCE`".to_owned());
            };
            Ok((
                Action::Remove {
                    source: (*source).to_owned(),
                },
                rest,
            ))
        }
        "refresh" => Ok((Action::Refresh, rest)),
        other => Err(format!(
            "`{other}` is not a link action; expected one of {:?}",
            Action::IMPLEMENTED
        )),
    }
}

/// Where the provider executable is named.
///
/// An environment variable rather than a settings field: a provider is a machine-local
/// installation detail, and a path in a shared settings file would name an executable that
/// does not exist on somebody else's machine — or, worse, one that does.
pub const PROVIDER_VARIABLE: &str = "NOSTDB_GITHUB_PROVIDER";

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

/// Reports what a change did, in whichever form was asked for.
fn write_change(
    verb: &str,
    change: &LinkChange,
    format: Format,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    match format {
        Format::Json | Format::Jsonl => {
            let mut entry = json!({
                "action": verb,
                "source": change.link.source.as_str(),
                "database_generation": change.generation.get(),
                "settings_updated": change.settings_updated,
                "nost_updated": change.nost_updated,
            });
            if let Some(alias) = &change.link.alias {
                entry["alias"] = json!(alias.as_str());
            }
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(&entry).unwrap_or_else(|_| entry.to_string())
            );
        }
        Format::Table | Format::Csv => {
            let _ = writeln!(
                out,
                "{verb} {}, generation {}",
                change.link, change.generation
            );
        }
    }
    // Which files moved is diagnostic rather than data, so it stays off stdout.
    if !change.settings_updated {
        let _ = writeln!(err, "note: the settings mirror already agreed");
    }
}

/// Runs a state-changing `link` action.
fn change(
    action: &Action,
    project: &Project,
    format: Format,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitClass {
    let outcome = match action {
        Action::Add { source, alias } => project
            .add_link(source, alias.as_deref())
            .map(|change| ("added", change)),
        Action::Remove { source } => project
            .remove_link(source)
            .map(|change| ("removed", change)),
        Action::Refresh => return refresh(project, format, out, err),
        Action::List | Action::Check => unreachable!("guarded by Action::writes"),
    };
    match outcome {
        Ok((verb, change)) => {
            write_change(verb, &change, format, out, err);
            ExitClass::Success
        }
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            ExitClass::for_project_error(&error)
        }
    }
}

/// Runs `nostdb link refresh`.
///
/// Resolution goes through a provider process, which this starts and stops. A local link
/// needs none, so a project with no remote link never launches one.
fn refresh(
    project: &Project,
    format: Format,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitClass {
    let program = std::env::var_os(PROVIDER_VARIABLE).map(PathBuf::from);
    let mut started: Option<ProviderClient<ProviderProcess>> = None;

    let outcomes = project.refresh_links(|locator| {
        if started.is_none() {
            // Started on first use rather than up front: a project whose links are all
            // local should not need a provider installed to be told it has nothing to do.
            let Some(program) = program.as_deref() else {
                return Err(format!(
                    "{PROVIDER_VARIABLE} names no provider executable, and {locator} needs one"
                ));
            };
            let process = ProviderProcess::start(program, &[])?;
            let mut client = ProviderClient::new(process);
            client.handshake().map_err(|error| error.to_string())?;
            started = Some(client);
        }
        let client = started.as_mut().ok_or("the provider did not start")?;
        // The credential is named in settings and resolved by the provider. Nothing here
        // holds a secret, which is why nothing here can leak one.
        let snapshot = client
            .resolve(locator, None)
            .map_err(|error| error.to_string())?;
        Ok(RefreshedSnapshot {
            commit: snapshot.snapshot,
            digest: None,
        })
    });

    let outcomes = match outcomes {
        Ok(outcomes) => outcomes,
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return ExitClass::for_project_error(&error);
        }
    };

    match format {
        Format::Json | Format::Jsonl => {
            let document = json!({
                "links": outcomes.iter().map(|outcome| match outcome {
                    RefreshOutcome::Advanced { source, from, to } => json!({
                        "source": source, "outcome": "advanced", "from": from, "to": to,
                    }),
                    RefreshOutcome::Unchanged { source, commit } => json!({
                        "source": source, "outcome": "unchanged", "commit": commit,
                    }),
                    RefreshOutcome::Unavailable { source, reason } => json!({
                        "source": source, "outcome": "unavailable", "reason": reason,
                    }),
                    RefreshOutcome::NotRemote { source } => json!({
                        "source": source, "outcome": "not_remote",
                    }),
                }).collect::<Vec<_>>(),
            });
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(&document).unwrap_or_else(|_| document.to_string())
            );
        }
        Format::Table | Format::Csv => {
            for outcome in &outcomes {
                match outcome {
                    RefreshOutcome::Advanced { source, from, to } => {
                        let _ = writeln!(
                            out,
                            "advanced   {source} {} -> {to}",
                            from.as_deref().unwrap_or("(none)")
                        );
                    }
                    RefreshOutcome::Unchanged { source, commit } => {
                        let _ = writeln!(out, "unchanged  {source} {commit}");
                    }
                    RefreshOutcome::Unavailable { source, .. } => {
                        let _ = writeln!(out, "unavailable {source}");
                    }
                    RefreshOutcome::NotRemote { source } => {
                        let _ = writeln!(out, "local      {source}");
                    }
                }
            }
        }
    }

    // An unreachable link keeps its declaration and whatever commit it had, so this is a
    // warning rather than a failure — the same reading `link list` gives a broken link.
    for outcome in outcomes.iter().filter(|outcome| outcome.is_unavailable()) {
        if let RefreshOutcome::Unavailable { source, reason } = outcome {
            let _ = writeln!(err, "warning: {source} could not be refreshed: {reason}");
        }
    }
    ExitClass::Success
}

/// Runs `nostdb link <ACTION>`.
pub fn run(
    action: &Action,
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
    if action.writes() {
        // A change does not resolve links first. Whether a source is reachable is a
        // separate question from whether it is declared, and refusing to declare an
        // unreachable one would make `link add` fail on a sibling not yet cloned.
        return change(action, &project, format, out, err);
    }
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
        Action::List | Action::Add { .. } | Action::Remove { .. } | Action::Refresh => {
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
            // Each implemented action parses once given whatever operands it needs.
            let words: Vec<&str> = match name {
                "add" | "remove" => vec![name, "./child"],
                _ => vec![name],
            };
            assert!(parse(&words).is_ok(), "{name}");
        }
        for name in Action::DEFERRED {
            assert!(
                parse(&[name]).is_err(),
                "{name} is not implemented, so it must not parse as one that is"
            );
        }
        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn refresh_is_an_action_now_and_writes() {
        // It was refused since Stage 7 because a local link is read live and has no
        // snapshot to advance. There is a remote source to advance one for now.
        let (action, rest) = parse(&["refresh", "--project", "/tmp"]).unwrap();
        assert_eq!(action, Action::Refresh);
        assert!(action.writes());
        assert_eq!(rest, ["--project", "/tmp"]);

        // `DEFERRED` used to be asserted empty here. It is empty now that `refresh` has landed,
        // which makes the assertion a compile-time tautology that clippy's `const_is_empty`
        // rejects, and the loop in `only_the_implemented_actions_are_read` already requires every
        // deferred action to fail to parse — so it keeps working if one ever returns, which a
        // const emptiness check would not.
    }

    #[test]
    fn add_reads_the_alias_the_way_the_language_writes_it() {
        assert_eq!(
            parse(&["add", "./child", "as", "child"]).unwrap().0,
            Action::Add {
                source: "./child".to_owned(),
                alias: Some("child".to_owned()),
            }
        );
        assert_eq!(
            parse(&["add", "./child"]).unwrap().0,
            Action::Add {
                source: "./child".to_owned(),
                alias: None,
            }
        );
        assert!(parse(&["add", "./child", "as"]).is_err());
        assert!(parse(&["add"]).is_err());
        assert!(parse(&["remove"]).is_err());
    }

    #[test]
    fn the_options_after_the_operands_are_left_for_the_shared_parser() {
        let (action, rest) = parse(&["add", "./child", "as", "child", "--format", "json"]).unwrap();
        assert!(action.writes());
        assert_eq!(rest, ["--format", "json"]);

        let (action, rest) = parse(&["list", "--project", "/tmp"]).unwrap();
        assert!(!action.writes());
        assert_eq!(rest, ["--project", "/tmp"]);
    }
}
