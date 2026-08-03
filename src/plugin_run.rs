//! Running an installed plugin, and refusing one that is not what was approved.
//!
//! [`crate::plugin_install`] recorded what a user agreed to so that something could later be
//! refused. This is the thing that refuses.
//!
//! # The order is the safety property
//!
//! Every check happens before the process exists, and the order matters rather than being
//! incidental: the digests are verified first, and only then is the installed manifest read for
//! the entrypoint, the declared actions, and the Engine range. Reading it earlier would be reading
//! whatever is on disk; reading it after the digest holds is reading the approved bytes.
//!
//! # The transport is the provider's, on purpose
//!
//! The published protocol uses the framing `PROVIDER_PROTOCOL.md` already defines, so this uses
//! the reader that implements it. A second framing would be a second set of framing bugs, and the
//! subtle one — a buffered line reader swallowing part of a content run — is worth solving once.
//! The type is named for where the framing came from rather than for what it carries.
//!
//! # This is not a sandbox
//!
//! A plugin runs as the user who invoked it. Every rule here is a rule about what the manager
//! hands over and what it accepts back, never a restraint on what the plugin can do.

use crate::exit::ExitClass;
use crate::plugin_install::{
    EngineRange, Installation, MANIFEST_NAME, Record, Scope, Version, plugins_directory,
    read_record, tree_digest,
};
use nostdb_core::provider::Transport;
use nostdb_core::sync::digest_bytes;
use std::path::{Path, PathBuf};

/// The protocol version this build speaks.
pub const PLUGIN_PROTOCOL_VERSION: u64 = 1;

/// The exchange media type version 1 defines.
pub const GRAPH_MEDIA_TYPE: &str = "application/vnd.nostdb.graph+json";

/// How long a handshake may take before a plugin is treated as unusable.
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Why an invocation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProtocolCode {
    /// A message's version is not one this build implements.
    Unsupported,
    /// A message is malformed, names an unknown kind, or carries a media type nothing reads.
    RequestInvalid,
    /// The action is not one this plugin implements.
    ActionUnknown,
    /// A handshake disagrees with what was approved.
    IdentityMismatch,
    /// An action needs a plugin that is not installed.
    Required,
    /// The action did not complete, or the plugin broke the protocol.
    Failed,
}

impl ProtocolCode {
    /// Every code, so a test can walk them.
    pub const ALL: [Self; 6] = [
        Self::Unsupported,
        Self::RequestInvalid,
        Self::ActionUnknown,
        Self::IdentityMismatch,
        Self::Required,
        Self::Failed,
    ];

    /// The symbolic name a refusal carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "PLUGIN_PROTOCOL_UNSUPPORTED",
            Self::RequestInvalid => "PLUGIN_REQUEST_INVALID",
            Self::ActionUnknown => "PLUGIN_ACTION_UNKNOWN",
            Self::IdentityMismatch => "PLUGIN_IDENTITY_MISMATCH",
            Self::Required => "PLUGIN_REQUIRED",
            Self::Failed => "PLUGIN_FAILED",
        }
    }

    /// The exit class a refusal reports.
    ///
    /// All of them are class 7. `docs/PRD.md` section 20.4 defines that class as a plugin being
    /// required or a plugin failing, and every condition here is one of those two — including a
    /// malformed message, because a plugin that cannot speak the protocol has failed at it.
    #[must_use]
    pub const fn exit_class(self) -> ExitClass {
        ExitClass::Plugin
    }
}

impl std::fmt::Display for ProtocolCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A refusal, with the reason a person reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunError {
    /// The code a script branches on.
    pub code: String,
    /// Why, for a person.
    pub reason: String,
    /// The class the process reports.
    pub class: ExitClass,
}

impl RunError {
    fn protocol(code: ProtocolCode, reason: impl Into<String>) -> Self {
        Self {
            code: code.as_str().to_owned(),
            reason: reason.into(),
            class: code.exit_class(),
        }
    }

    /// A refusal the installation contract owns, reported with its own code and class.
    fn install(error: crate::plugin_install::InstallError) -> Self {
        Self {
            class: error.code.exit_class(),
            code: error.code.as_str().to_owned(),
            reason: error.reason,
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.reason)
    }
}

impl std::error::Error for RunError {}

/// What a plugin said about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handshake {
    /// The name it answers to.
    pub plugin: String,
    /// The version it reports, which is informational and never compared.
    pub plugin_version: String,
    /// The actions it implements.
    pub actions: Vec<String>,
}

/// The artifact a plugin is handed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exchange {
    /// What is inside it.
    pub media_type: String,
    /// Where it is, absolute.
    pub path: PathBuf,
    /// Its exact length.
    pub bytes: u64,
    /// The digest a plugin verifies before interpreting it.
    pub content_digest: String,
}

/// What an invocation produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invoked {
    /// `complete` or `partial`.
    pub status: String,
    /// What the plugin says it wrote, relative to the output directory.
    pub outputs: Vec<String>,
}

impl Invoked {
    /// Reports whether the plugin finished everything it was asked to do.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.status == "complete"
    }
}

/// What the pre-flight checks established, and what an invocation needs from them.
#[derive(Clone, Debug)]
pub struct Ready {
    /// The record entry that authorized this.
    pub installation: Installation,
    /// Where the plugin's files are.
    pub directory: PathBuf,
    /// The entrypoint, as an argument vector resolved against `directory`.
    pub command: Vec<String>,
    /// The actions the approved manifest declared.
    pub declared_actions: Vec<String>,
    /// Whether the approval grants graph access.
    pub graph_read: bool,
    /// The approved output globs.
    pub output_paths: Vec<String>,
}

/// Recomputes both recorded digests over an installed directory.
///
/// # Errors
///
/// Returns a reason when the directory cannot be walked or the manifest cannot be read.
pub fn recompute_digests(directory: &Path) -> Result<(String, String), String> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect(directory, directory, &mut files)?;

    let manifest = files
        .iter()
        .find(|(path, _)| path == MANIFEST_NAME)
        .ok_or_else(|| format!("{} holds no {MANIFEST_NAME}", directory.display()))?;
    let manifest_digest = digest_bytes(&manifest.1).to_string();
    Ok((manifest_digest, tree_digest(&files)))
}

/// Walks a directory into plugin-relative path and content pairs.
fn collect(root: &Path, at: &Path, into: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
    let entries = std::fs::read_dir(at).map_err(|error| format!("{}: {error}", at.display()))?;
    // Sorted, because a directory's iteration order is the filesystem's business and a digest
    // that depended on it would differ between two machines holding identical files.
    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()
        .map_err(|error| format!("{}: {error}", at.display()))?;
    paths.sort();

    for path in paths {
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if metadata.is_dir() {
            collect(root, &path, into)?;
            continue;
        }
        if metadata.file_type().is_symlink() {
            // A symlink was never installed: every entry came from a fetched tree as bytes. One
            // here is something that arrived afterwards, and following it would digest a file
            // outside the plugin.
            return Err(format!(
                "{} is a symbolic link, which no installation writes",
                path.display()
            ));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("{} is not under {}", path.display(), root.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        into.push((relative, bytes));
    }
    Ok(())
}

/// Every check that happens before the plugin process exists.
///
/// The order is the contract's, and each step exists to make the next one safe. In particular the
/// digests are verified before the installed manifest is read, so what is read is the approved
/// bytes rather than whatever is on disk.
///
/// # Errors
///
/// Returns the refusal, carrying the code and the exit class the contract assigns it.
pub fn preflight(
    record: &Record,
    scope: Scope,
    root: &Path,
    name: &str,
    action: &str,
    engine: &Version,
) -> Result<Ready, RunError> {
    // 1. Installed.
    let installation = record.find(name).cloned().ok_or_else(|| {
        RunError::protocol(
            ProtocolCode::Required,
            format!(
                "{name} is not installed for {}; install it with \
                 `nostdb plugin add <source>` or see `nostdb plugin list`",
                scope.as_str()
            ),
        )
    })?;

    let directory = plugins_directory(root).join(name);

    // 2. Both recorded digests still hold. Nothing below reads the manifest until this passes.
    let (manifest_digest, tree) = recompute_digests(&directory).map_err(|reason| {
        RunError::protocol(
            ProtocolCode::Failed,
            format!("{name} cannot be verified: {reason}"),
        )
    })?;
    if manifest_digest != installation.manifest_digest || tree != installation.tree_digest {
        let which = if manifest_digest == installation.manifest_digest {
            "its code changed behind an unchanged manifest"
        } else {
            "its manifest changed"
        };
        return Err(RunError::install(
            crate::plugin_install::InstallError::at_run(format!(
                "{name} is not what was approved: {which}"
            )),
        ));
    }

    // The manifest is now the approved manifest, byte for byte.
    let text = std::fs::read_to_string(directory.join(MANIFEST_NAME)).map_err(|error| {
        RunError::protocol(
            ProtocolCode::Failed,
            format!("{name}: the verified manifest cannot be read: {error}"),
        )
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        RunError::protocol(
            ProtocolCode::Failed,
            format!("{name}: the verified manifest is not JSON: {error}"),
        )
    })?;

    // 3. The Engine range still admits this build.
    let declared = manifest["nostdb"].as_str().unwrap_or_default();
    let range = EngineRange::parse(declared).map_err(|reason| {
        RunError::protocol(
            ProtocolCode::Failed,
            format!("{name}: the verified manifest states no readable range: {reason}"),
        )
    })?;
    if !range.admits(engine) {
        return Err(RunError::install(
            crate::plugin_install::InstallError::incompatible(format!(
                "{name} declares `{declared}` and this build is {engine}"
            )),
        ));
    }

    // 4. The action is one the approved manifest declared.
    let declared_actions: Vec<String> = manifest["actions"]
        .as_array()
        .map(|actions| {
            actions
                .iter()
                .filter_map(|entry| entry["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if !declared_actions.iter().any(|declared| declared == action) {
        return Err(RunError::protocol(
            ProtocolCode::ActionUnknown,
            format!("{name} declares {declared_actions:?} and was asked for `{action}`"),
        ));
    }

    let command: Vec<String> = manifest["entrypoint"]["command"]
        .as_array()
        .map(|vector| {
            vector
                .iter()
                .filter_map(|part| part.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if command.is_empty() {
        return Err(RunError::protocol(
            ProtocolCode::Failed,
            format!("{name}: the verified manifest states no entrypoint vector"),
        ));
    }

    Ok(Ready {
        graph_read: installation.approved_permissions["graph_read"]
            .as_bool()
            .unwrap_or(false),
        output_paths: installation.approved_permissions["output_paths"]
            .as_array()
            .map(|globs| {
                globs
                    .iter()
                    .filter_map(|glob| glob.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        installation,
        directory,
        command,
        declared_actions,
    })
}

/// Finds an installation in project scope, then global.
///
/// Project first, for the reason a project-local Engine is preferred to a global one: a project
/// that pinned something did so on purpose.
///
/// # Errors
///
/// Returns the reason a record could not be read.
pub fn locate(
    project: Option<&Path>,
    home: Option<&Path>,
    name: &str,
) -> Result<Option<(Scope, PathBuf, Record)>, RunError> {
    for (scope, root) in [(Scope::Project, project), (Scope::Global, home)] {
        let Some(root) = root else { continue };
        let record = read_record(root, scope).map_err(RunError::install)?;
        if record.find(name).is_some() {
            return Ok(Some((scope, root.to_owned(), record)));
        }
    }
    Ok(None)
}

/// Reads and checks a handshake reply against what was approved.
///
/// # Errors
///
/// Returns [`ProtocolCode::Unsupported`] for another protocol version, [`ProtocolCode::Failed`] for
/// a reply that is not a handshake, and [`ProtocolCode::IdentityMismatch`] when the reply claims a
/// different name or an action nobody approved.
pub fn check_handshake(reply: &str, ready: &Ready) -> Result<Handshake, RunError> {
    let message: serde_json::Value = serde_json::from_str(reply).map_err(|error| {
        RunError::protocol(
            ProtocolCode::Failed,
            format!("the reply is not JSON: {error}"),
        )
    })?;

    match message["plugin_protocol_version"].as_u64() {
        Some(PLUGIN_PROTOCOL_VERSION) => {}
        Some(found) => {
            return Err(RunError::protocol(
                ProtocolCode::Unsupported,
                format!("the plugin speaks plugin_protocol_version {found}"),
            ));
        }
        None => {
            return Err(RunError::protocol(
                ProtocolCode::Failed,
                "the reply states no plugin_protocol_version",
            ));
        }
    }

    if message["reply"].as_str() != Some("handshake") {
        // A plugin answering something else before agreeing a version has guessed what was asked.
        return Err(RunError::protocol(
            ProtocolCode::Failed,
            format!("expected a handshake reply, got {}", message["reply"]),
        ));
    }

    let plugin = message["plugin"].as_str().unwrap_or_default().to_owned();
    if plugin != ready.installation.name {
        return Err(RunError::protocol(
            ProtocolCode::IdentityMismatch,
            format!(
                "the process answers to `{plugin}` and `{}` was installed",
                ready.installation.name
            ),
        ));
    }

    let actions: Vec<String> = message["actions"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|action| action.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    // Claiming more than was approved is the refusal. Claiming fewer is a plugin implementing less
    // than it advertised, and invoking a missing one is refused later by name.
    for action in &actions {
        if !ready.declared_actions.contains(action) {
            return Err(RunError::protocol(
                ProtocolCode::IdentityMismatch,
                format!(
                    "the process claims `{action}`, which the approved manifest never declared"
                ),
            ));
        }
    }

    Ok(Handshake {
        plugin,
        plugin_version: message["plugin_version"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        actions,
    })
}

/// Reads and checks an invoke reply.
///
/// # Errors
///
/// Returns [`ProtocolCode::Failed`] for a malformed reply, an unknown status, or an output that is
/// not one the approval permits, and whatever the plugin refused with when it refused.
pub fn check_invoke(reply: &str, ready: &Ready) -> Result<Invoked, RunError> {
    let message: serde_json::Value = serde_json::from_str(reply).map_err(|error| {
        RunError::protocol(
            ProtocolCode::Failed,
            format!("the reply is not JSON: {error}"),
        )
    })?;

    if message["plugin_protocol_version"].as_u64() != Some(PLUGIN_PROTOCOL_VERSION) {
        return Err(RunError::protocol(
            ProtocolCode::Unsupported,
            format!(
                "the reply states plugin_protocol_version {}",
                message["plugin_protocol_version"]
            ),
        ));
    }

    // A refusal is a reply, so it is read here rather than inferred from an exit status.
    if message["reply"].as_str() == Some("error") {
        let code = message["code"].as_str().unwrap_or_default();
        let stated = message["message"].as_str().unwrap_or("no reason was given");
        let known = ProtocolCode::ALL
            .iter()
            .find(|candidate| candidate.as_str() == code);
        return Err(match known {
            Some(code) => RunError::protocol(*code, stated.to_owned()),
            // A code outside the registry is one no caller can look up, so the refusal becomes one
            // that can be: the plugin failed, and the message says what it claimed.
            None => RunError::protocol(
                ProtocolCode::Failed,
                format!("the plugin refused with the unregistered code `{code}`: {stated}"),
            ),
        });
    }

    if message["reply"].as_str() != Some("invoke") {
        return Err(RunError::protocol(
            ProtocolCode::Failed,
            format!("expected an invoke reply, got {}", message["reply"]),
        ));
    }

    let status = message["status"].as_str().unwrap_or_default().to_owned();
    if status != "complete" && status != "partial" {
        return Err(RunError::protocol(
            ProtocolCode::Failed,
            format!("`{status}` is not a status; expected complete or partial"),
        ));
    }

    let Some(listed) = message["outputs"].as_array() else {
        // An absent list is not an empty one: a plugin that wrote nothing says so with `[]`.
        return Err(RunError::protocol(
            ProtocolCode::Failed,
            "the reply states no outputs list",
        ));
    };

    let mut outputs = Vec::with_capacity(listed.len());
    for value in listed {
        let Some(output) = value.as_str() else {
            return Err(RunError::protocol(
                ProtocolCode::Failed,
                "an output is not a string",
            ));
        };
        if output.starts_with('/') || output.split('/').any(|part| part == "..") {
            return Err(RunError::protocol(
                ProtocolCode::Failed,
                format!("`{output}` is absolute or escapes the output directory"),
            ));
        }
        if !ready
            .output_paths
            .iter()
            .any(|glob| matches_glob(glob, output))
        {
            return Err(RunError::protocol(
                ProtocolCode::Failed,
                format!(
                    "`{output}` matches none of the approved output paths {:?}",
                    ready.output_paths
                ),
            ));
        }
        outputs.push(output.to_owned());
    }

    Ok(Invoked { status, outputs })
}

/// Matches an output against an approved glob.
///
/// The globs an author writes are project-relative, `.nostdb/out/**`, and an output is relative to
/// the output directory, `view.html`. So a glob's leading directories are matched against the
/// output directory the manager chose, and what a plugin reports is matched against the tail.
///
/// Deliberately small: `*` matches within one segment and `**` matches across segments. A fuller
/// glob dialect would be a second thing to get wrong, and every published example is one of these
/// two forms.
#[must_use]
pub fn matches_glob(glob: &str, output: &str) -> bool {
    let tail = glob.rsplit('/').next().unwrap_or(glob);
    match tail {
        "**" | "*" => true,
        pattern => segment_matches(pattern, output.rsplit('/').next().unwrap_or(output)),
    }
}

fn segment_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((before, after)) => {
            name.len() >= before.len() + after.len()
                && name.starts_with(before)
                && name.ends_with(after)
        }
    }
}

/// The request line that opens a conversation.
#[must_use]
pub fn handshake_request() -> String {
    format!(r#"{{"plugin_protocol_version":{PLUGIN_PROTOCOL_VERSION},"request":"handshake"}}"#)
}

/// The request line that invokes an action.
///
/// `exchange` is omitted when the approval does not grant `graph_read`. That absence is the
/// permission meaning something: a manager that supplied one anyway would have made the field
/// decorative, and a user who declined graph access would have been told something untrue.
#[must_use]
pub fn invoke_request(
    action: &str,
    output_directory: &Path,
    exchange: Option<&Exchange>,
) -> String {
    let mut request = serde_json::json!({
        "plugin_protocol_version": PLUGIN_PROTOCOL_VERSION,
        "request": "invoke",
        "action": action,
        "output_directory": output_directory.display().to_string(),
        "options": {},
    });
    if let Some(exchange) = exchange {
        request["exchange"] = serde_json::json!({
            "kind": "artifact",
            "media_type": exchange.media_type,
            "path": exchange.path.display().to_string(),
            "bytes": exchange.bytes,
            "content_digest": exchange.content_digest,
        });
    }
    request.to_string()
}

/// Runs one invocation over an agreed transport.
///
/// # Errors
///
/// Returns the refusal. A transport failure is [`ProtocolCode::Failed`], because a plugin whose
/// stream broke has failed at the protocol whatever its intent was.
pub fn converse<T: Transport>(
    transport: &mut T,
    ready: &Ready,
    action: &str,
    output_directory: &Path,
    exchange: Option<&Exchange>,
) -> Result<(Handshake, Invoked), RunError> {
    let broken = |error: String| RunError::protocol(ProtocolCode::Failed, error);

    transport.send(&handshake_request()).map_err(broken)?;
    let reply = transport.receive().map_err(broken)?;
    let handshake = check_handshake(&reply, ready)?;

    // Asked for after the handshake, and refused here rather than by the plugin, because the
    // approved manifest is what says which actions exist and a plugin implementing fewer is
    // legitimate.
    if !handshake.actions.iter().any(|known| known == action) {
        return Err(RunError::protocol(
            ProtocolCode::ActionUnknown,
            format!(
                "{} implements {:?} and was asked for `{action}`",
                handshake.plugin, handshake.actions
            ),
        ));
    }

    transport
        .send(&invoke_request(action, output_directory, exchange))
        .map_err(broken)?;
    let reply = transport.receive().map_err(broken)?;
    let invoked = check_invoke(&reply, ready)?;
    Ok((handshake, invoked))
}

/// Where a scope's root is, or `None` when this session has no such scope.
fn roots(from: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let project = nostdb_core::project::Project::is_configured(from).then(|| from.to_owned());
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    (project, home)
}

/// The scopes to look in, narrowed when the invocation named one.
fn scopes(requested: Option<Scope>, from: &Path) -> Vec<(Scope, PathBuf)> {
    let (project, home) = roots(from);
    // Project first, for the reason a project-local Engine is preferred to a global one: a project
    // that pinned something did so on purpose.
    [(Scope::Project, project), (Scope::Global, home)]
        .into_iter()
        .filter(|(scope, _)| requested.is_none_or(|wanted| wanted == *scope))
        .filter_map(|(scope, root)| root.map(|root| (scope, root)))
        .collect()
}

/// Runs `nostdb plugin list`.
///
/// Reads the record rather than the plugin directories. A listing built by looking at what is on
/// disk would report a directory somebody copied in as an installation, which is the one thing the
/// record exists to distinguish.
pub fn list(
    requested: Option<Scope>,
    from: &Path,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> ExitClass {
    let mut found = 0usize;
    for (scope, root) in scopes(requested, from) {
        let record = match read_record(&root, scope) {
            Ok(record) => record,
            Err(error) => {
                let _ = writeln!(err, "{}: {}", error.code, error.reason);
                return error.code.exit_class();
            }
        };
        for entry in record.installed() {
            found += 1;
            let _ = writeln!(
                out,
                "{}  {}  {}  {}",
                entry.name,
                entry.plugin_version,
                scope.as_str(),
                &entry.commit[..12.min(entry.commit.len())]
            );
            let _ = writeln!(out, "    {}", entry.repository);
            let _ = writeln!(out, "    permissions: {}", entry.approved_permissions);
        }
    }
    if found == 0 {
        // On stderr, because an empty listing has no data and a caller piping this should receive
        // nothing rather than a sentence.
        let _ = writeln!(err, "nothing is installed");
    }
    ExitClass::Success
}

/// Runs `nostdb plugin remove`.
///
/// The record entry goes first and the directory second. A directory removed before the record
/// would leave a record naming files that are gone, which is the state every check here treats as
/// tampering.
pub fn remove(
    name: &str,
    requested: Option<Scope>,
    from: &Path,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> ExitClass {
    for (scope, root) in scopes(requested, from) {
        let mut record = match read_record(&root, scope) {
            Ok(record) => record,
            Err(error) => {
                let _ = writeln!(err, "{}: {}", error.code, error.reason);
                return error.code.exit_class();
            }
        };
        if record.find(name).is_none() {
            continue;
        }
        record.remove(name);
        if let Err(error) = crate::plugin_install::write_record(&record, &root) {
            let _ = writeln!(err, "{}: {}", error.code, error.reason);
            return error.code.exit_class();
        }
        let directory = plugins_directory(&root).join(name);
        if let Err(error) = std::fs::remove_dir_all(&directory) {
            if error.kind() != std::io::ErrorKind::NotFound {
                // The record is already updated, so this is reported and not rolled back: a
                // half-removed installation the record still names is worse than files nothing
                // names.
                let _ = writeln!(
                    err,
                    "the record no longer names {name}, and {} could not be removed: {error}",
                    directory.display()
                );
                return ExitClass::Io;
            }
        }
        let _ = writeln!(out, "removed {name} from {}", scope.as_str());
        return ExitClass::Success;
    }

    let _ = writeln!(
        err,
        "{}: {name} is not installed; see `nostdb plugin list`",
        ProtocolCode::Required
    );
    ProtocolCode::Required.exit_class()
}

/// Runs `nostdb plugin run`.
pub fn run(
    name: &str,
    action: &str,
    requested: Option<Scope>,
    from: &Path,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> ExitClass {
    let engine = match crate::plugin_install::engine_version() {
        Ok(version) => version,
        Err(reason) => {
            let _ = writeln!(err, "this build reports no readable version: {reason}");
            return ExitClass::Internal;
        }
    };

    let mut located = None;
    for (scope, root) in scopes(requested, from) {
        let record = match read_record(&root, scope) {
            Ok(record) => record,
            Err(error) => {
                let _ = writeln!(err, "{}: {}", error.code, error.reason);
                return error.code.exit_class();
            }
        };
        if record.find(name).is_some() {
            located = Some((scope, root, record));
            break;
        }
    }
    let Some((scope, root, record)) = located else {
        let _ = writeln!(
            err,
            "{}: {name} is not installed; install it with `nostdb plugin add <source>` or see \
             `nostdb plugin list`",
            ProtocolCode::Required
        );
        return ProtocolCode::Required.exit_class();
    };

    // The action may be omitted when the approved manifest declares exactly one. Resolving it needs
    // the manifest, and the manifest may not be read before its digest holds — so the pre-flight
    // runs first with whatever was asked for, and an empty action is filled in from what it found.
    let probe = if action.is_empty() { "" } else { action };
    let ready = match preflight(&record, scope, &root, name, probe, &engine) {
        Ok(ready) => ready,
        Err(error) if action.is_empty() && error.code == ProtocolCode::ActionUnknown.as_str() => {
            // The pre-flight refused because no action was named. Everything before that step
            // passed, so the manifest is verified and its declared actions are known from the
            // message; rather than parse that back, run it again once the single action is known.
            match sole_action(&record, scope, &root, name, &engine) {
                Ok(only) => match preflight(&record, scope, &root, name, &only, &engine) {
                    Ok(ready) => ready,
                    Err(error) => {
                        let _ = writeln!(err, "{error}");
                        return error.class;
                    }
                },
                Err(error) => {
                    let _ = writeln!(err, "{error}");
                    return error.class;
                }
            }
        }
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return error.class;
        }
    };
    let action = if action.is_empty() {
        ready.declared_actions.first().cloned().unwrap_or_default()
    } else {
        action.to_owned()
    };

    let output_directory = root.join(".nostdb").join("out");
    if let Err(error) = std::fs::create_dir_all(&output_directory) {
        let _ = writeln!(
            err,
            "{} could not be created: {error}",
            output_directory.display()
        );
        return ExitClass::Io;
    }

    // The exchange is built only when the approval grants graph access. A manager that built one
    // anyway and withheld it would have read the graph for nothing.
    let handover = if ready.graph_read {
        match load_graph_for(&root) {
            Ok(graph) => match hand_over(&graph, name) {
                Ok(handover) => Some(handover),
                Err(reason) => {
                    let _ = writeln!(err, "the exchange could not be written: {reason}");
                    return ExitClass::Io;
                }
            },
            Err(reason) => {
                let _ = writeln!(err, "{reason}");
                return ExitClass::Io;
            }
        }
    } else {
        None
    };

    let program = ready.directory.join(&ready.command[0]);
    let arguments: Vec<&str> = ready.command[1..].iter().map(String::as_str).collect();
    let process = match nostdb_core::provider_process::ProviderProcess::start(&program, &arguments)
    {
        Ok(process) => process,
        Err(reason) => {
            let _ = writeln!(err, "{}: {reason}", ProtocolCode::Failed);
            return ProtocolCode::Failed.exit_class();
        }
    };
    let mut transport = process;

    let outcome = converse(
        &mut transport,
        &ready,
        &action,
        &output_directory,
        handover.as_ref().map(Handover::exchange),
    );

    match outcome {
        Ok((handshake, invoked)) => {
            let _ = writeln!(
                out,
                "{} {} ran `{action}`: {}",
                handshake.plugin, handshake.plugin_version, invoked.status
            );
            for output in &invoked.outputs {
                let _ = writeln!(out, "    {}", output_directory.join(output).display());
            }
            if invoked.is_complete() {
                ExitClass::Success
            } else {
                // Partial work is kept and reported as a plugin failure, because reporting it as
                // success is what makes a status field worthless.
                let _ = writeln!(
                    err,
                    "{}: {} reported partial work",
                    ProtocolCode::Failed,
                    handshake.plugin
                );
                ProtocolCode::Failed.exit_class()
            }
        }
        Err(error) => {
            let _ = writeln!(err, "{error}");
            error.class
        }
    }
}

/// The one action an approved manifest declares, when it declares exactly one.
fn sole_action(
    record: &Record,
    scope: Scope,
    root: &Path,
    name: &str,
    engine: &Version,
) -> Result<String, RunError> {
    // A probe that reaches step 4 and fails there has already proven the digests and the range, so
    // the declared actions it reports are the approved ones.
    let declared = match preflight(record, scope, root, name, "\u{0}", engine) {
        Ok(ready) => ready.declared_actions,
        Err(error) if error.code == ProtocolCode::ActionUnknown.as_str() => {
            // The message names them, and re-deriving the list is cheaper than parsing a sentence.
            let directory = plugins_directory(root).join(name);
            let text = std::fs::read_to_string(directory.join(MANIFEST_NAME))
                .map_err(|error| RunError::protocol(ProtocolCode::Failed, error.to_string()))?;
            let manifest: serde_json::Value = serde_json::from_str(&text)
                .map_err(|error| RunError::protocol(ProtocolCode::Failed, error.to_string()))?;
            manifest["actions"]
                .as_array()
                .map(|actions| {
                    actions
                        .iter()
                        .filter_map(|entry| entry["name"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        }
        Err(error) => return Err(error),
    };

    match declared.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(RunError::protocol(
            ProtocolCode::ActionUnknown,
            format!("{name} declares no actions"),
        )),
        many => Err(RunError::protocol(
            ProtocolCode::ActionUnknown,
            format!("{name} declares {many:?}; name the one to run"),
        )),
    }
}

/// Opens the project's graph for the exchange.
fn load_graph_for(root: &Path) -> Result<nostdb_core::encoding::Graph, String> {
    let project = nostdb_core::project::Project::open(root, None)
        .map_err(|error| format!("{root:?} is not a readable project: {error}"))?;
    let database = project
        .open_database()
        .map_err(|error| format!("the database could not be opened: {error}"))?;
    nostdb_core::encoding::read_graph(&database)
        .map_err(|error| format!("the graph could not be read: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installation(name: &str, permissions: serde_json::Value) -> Installation {
        Installation {
            name: name.to_owned(),
            repository: "https://github.com/e/v".to_owned(),
            commit: "a".repeat(40),
            subdirectory: None,
            manifest_digest: format!("sha256:{}", "1".repeat(64)),
            tree_digest: format!("sha256:{}", "2".repeat(64)),
            scope: Scope::Project,
            manifest_version: 1,
            plugin_version: "1.0.0".to_owned(),
            approved_permissions: permissions,
        }
    }

    fn ready(actions: &[&str], outputs: &[&str], graph_read: bool) -> Ready {
        let permissions = serde_json::json!({
            "graph_read": graph_read,
            "database_write": false,
            "output_paths": outputs,
            "network_hosts": [],
        });
        Ready {
            installation: installation("org.example.tool", permissions),
            directory: PathBuf::from("/p/.nostdb/plugins/org.example.tool"),
            command: vec!["bin/tool".to_owned()],
            declared_actions: actions.iter().map(|a| (*a).to_owned()).collect(),
            graph_read,
            output_paths: outputs.iter().map(|o| (*o).to_owned()).collect(),
        }
    }

    #[test]
    fn every_code_is_distinct_and_reports_class_seven() {
        let names: std::collections::BTreeSet<&str> =
            ProtocolCode::ALL.iter().map(|code| code.as_str()).collect();
        assert_eq!(names.len(), ProtocolCode::ALL.len());
        for code in ProtocolCode::ALL {
            assert_eq!(code.exit_class(), ExitClass::Plugin, "{code}");
        }
    }

    #[test]
    fn a_handshake_agreeing_with_the_approval_is_accepted() {
        let ready = ready(&["view", "report"], &[], false);
        let reply = r#"{"plugin_protocol_version":1,"reply":"handshake","plugin":"org.example.tool","plugin_version":"1.0.0","actions":["view"]}"#;
        let handshake = check_handshake(reply, &ready).expect("accepted");
        assert_eq!(handshake.plugin, "org.example.tool");
        // Fewer actions than approved is legitimate.
        assert_eq!(handshake.actions, ["view"]);
    }

    #[test]
    fn a_handshake_claiming_an_unapproved_action_is_refused() {
        let ready = ready(&["view"], &[], false);
        let reply = r#"{"plugin_protocol_version":1,"reply":"handshake","plugin":"org.example.tool","plugin_version":"1.0.0","actions":["view","write_everything"]}"#;
        let error = check_handshake(reply, &ready).expect_err("refused");
        assert_eq!(error.code, "PLUGIN_IDENTITY_MISMATCH");
        assert!(
            error.reason.contains("write_everything"),
            "{}",
            error.reason
        );
    }

    #[test]
    fn a_handshake_answering_to_another_name_is_refused() {
        let ready = ready(&["view"], &[], false);
        let reply = r#"{"plugin_protocol_version":1,"reply":"handshake","plugin":"org.other.tool","plugin_version":"1.0.0","actions":["view"]}"#;
        assert_eq!(
            check_handshake(reply, &ready).expect_err("refused").code,
            "PLUGIN_IDENTITY_MISMATCH"
        );
    }

    #[test]
    fn a_plugin_version_that_differs_is_not_a_mismatch() {
        // It is what the process says it is, and never compared. An edited manifest is what the
        // digests detect.
        let ready = ready(&["view"], &[], false);
        let reply = r#"{"plugin_protocol_version":1,"reply":"handshake","plugin":"org.example.tool","plugin_version":"9.9.9","actions":["view"]}"#;
        assert_eq!(
            check_handshake(reply, &ready)
                .expect("accepted")
                .plugin_version,
            "9.9.9"
        );
    }

    #[test]
    fn another_protocol_version_is_refused_before_anything_is_invoked() {
        let ready = ready(&["view"], &[], false);
        let reply = r#"{"plugin_protocol_version":2,"reply":"handshake","plugin":"org.example.tool","plugin_version":"1.0.0","actions":["view"]}"#;
        assert_eq!(
            check_handshake(reply, &ready).expect_err("refused").code,
            "PLUGIN_PROTOCOL_UNSUPPORTED"
        );
    }

    #[test]
    fn a_plugin_that_answers_something_else_first_has_guessed() {
        let ready = ready(&["view"], &[], false);
        let reply =
            r#"{"plugin_protocol_version":1,"reply":"invoke","status":"complete","outputs":[]}"#;
        assert_eq!(
            check_handshake(reply, &ready).expect_err("refused").code,
            "PLUGIN_FAILED"
        );
    }

    #[test]
    fn an_output_outside_the_approval_fails_the_invocation() {
        let ready = ready(&["view"], &[".nostdb/out/**"], true);
        for outputs in [r#"["../../etc/passwd"]"#, r#"["/etc/passwd"]"#, r#"[42]"#] {
            let reply = format!(
                r#"{{"plugin_protocol_version":1,"reply":"invoke","status":"complete","outputs":{outputs}}}"#
            );
            assert_eq!(
                check_invoke(&reply, &ready).expect_err("refused").code,
                "PLUGIN_FAILED",
                "{outputs}"
            );
        }
    }

    #[test]
    fn an_output_matching_no_approved_glob_is_refused() {
        let ready = ready(&["view"], &["*.html"], true);
        let reply = r#"{"plugin_protocol_version":1,"reply":"invoke","status":"complete","outputs":["view.bin"]}"#;
        let error = check_invoke(reply, &ready).expect_err("refused");
        assert_eq!(error.code, "PLUGIN_FAILED");
        assert!(
            error.reason.contains("approved output paths"),
            "{}",
            error.reason
        );
    }

    #[test]
    fn a_partial_invocation_keeps_what_it_reported() {
        let ready = ready(&["view"], &[".nostdb/out/**"], true);
        let reply = r#"{"plugin_protocol_version":1,"reply":"invoke","status":"partial","outputs":["view.data.bin"]}"#;
        let invoked = check_invoke(reply, &ready).expect("read");
        assert!(!invoked.is_complete());
        assert_eq!(invoked.outputs, ["view.data.bin"]);
    }

    #[test]
    fn an_absent_outputs_list_is_not_an_empty_one() {
        let ready = ready(&["view"], &[], true);
        let reply = r#"{"plugin_protocol_version":1,"reply":"invoke","status":"complete"}"#;
        assert_eq!(
            check_invoke(reply, &ready).expect_err("refused").code,
            "PLUGIN_FAILED"
        );
        let empty =
            r#"{"plugin_protocol_version":1,"reply":"invoke","status":"complete","outputs":[]}"#;
        assert!(
            check_invoke(empty, &ready)
                .expect("read")
                .outputs
                .is_empty()
        );
    }

    #[test]
    fn an_unknown_status_is_refused() {
        let ready = ready(&["view"], &[], true);
        let reply =
            r#"{"plugin_protocol_version":1,"reply":"invoke","status":"done","outputs":[]}"#;
        assert_eq!(
            check_invoke(reply, &ready).expect_err("refused").code,
            "PLUGIN_FAILED"
        );
    }

    #[test]
    fn a_refusal_is_read_from_the_reply_and_keeps_its_code() {
        let ready = ready(&["view"], &[], true);
        let reply = r#"{"plugin_protocol_version":1,"reply":"error","code":"PLUGIN_ACTION_UNKNOWN","message":"only view"}"#;
        let error = check_invoke(reply, &ready).expect_err("refused");
        assert_eq!(error.code, "PLUGIN_ACTION_UNKNOWN");
        assert_eq!(error.reason, "only view");
    }

    #[test]
    fn a_refusal_with_an_unregistered_code_becomes_one_a_caller_can_look_up() {
        let ready = ready(&["view"], &[], true);
        let reply = r#"{"plugin_protocol_version":1,"reply":"error","code":"PLUGIN_MADE_UP","message":"no"}"#;
        let error = check_invoke(reply, &ready).expect_err("refused");
        assert_eq!(error.code, "PLUGIN_FAILED");
        assert!(error.reason.contains("PLUGIN_MADE_UP"), "{}", error.reason);
    }

    #[test]
    fn an_invoke_request_omits_the_exchange_when_graph_read_was_not_approved() {
        let directory = Path::new("/p/.nostdb/out");
        let without = invoke_request("view", directory, None);
        assert!(!without.contains("exchange"), "{without}");

        let exchange = Exchange {
            media_type: GRAPH_MEDIA_TYPE.to_owned(),
            path: PathBuf::from("/tmp/x/graph.json"),
            bytes: 12,
            content_digest: format!("sha256:{}", "3".repeat(64)),
        };
        let with = invoke_request("view", directory, Some(&exchange));
        assert!(with.contains(GRAPH_MEDIA_TYPE), "{with}");
        assert!(with.contains("\"kind\":\"artifact\""), "{with}");
    }

    #[test]
    fn a_request_is_one_line_of_json() {
        // The framing is line-delimited, so a request carrying a newline would be two messages.
        let directory = Path::new("/p/.nostdb/out");
        for request in [handshake_request(), invoke_request("view", directory, None)] {
            assert!(!request.contains('\n'), "{request}");
            serde_json::from_str::<serde_json::Value>(&request).expect("valid JSON");
        }
    }

    #[test]
    fn a_glob_matches_within_and_across_segments() {
        assert!(matches_glob(".nostdb/out/**", "view.html"));
        assert!(matches_glob(".nostdb/out/*", "view.html"));
        assert!(matches_glob("*.html", "view.html"));
        assert!(!matches_glob("*.html", "view.bin"));
        assert!(matches_glob("view.html", "view.html"));
        assert!(!matches_glob("view.html", "other.html"));
    }

    #[test]
    fn digests_are_recomputed_over_the_installed_directory() {
        let mut base = std::env::temp_dir();
        base.push(format!("nostdb-run-digest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("bin")).expect("scratch");
        std::fs::write(base.join(MANIFEST_NAME), b"{}").expect("manifest");
        std::fs::write(base.join("bin").join("tool"), b"binary").expect("entrypoint");

        let (manifest, tree) = recompute_digests(&base).expect("computed");
        assert_eq!(manifest, digest_bytes(b"{}").to_string());
        assert_eq!(
            tree,
            tree_digest(&[
                (MANIFEST_NAME.to_owned(), b"{}".to_vec()),
                ("bin/tool".to_owned(), b"binary".to_vec()),
            ])
        );

        // A file added afterwards moves the tree digest and leaves the manifest digest alone,
        // which is the distinction the two digests exist to make.
        std::fs::write(base.join("scratch.tmp"), b"written at run time").expect("scratch file");
        let (again, moved) = recompute_digests(&base).expect("computed");
        assert_eq!(again, manifest);
        assert_ne!(moved, tree);

        let _ = std::fs::remove_dir_all(&base);
    }
}

/// The exchange artifact, and the temporary directory holding it.
///
/// Removed when this is dropped, including when the invocation failed. An artifact left behind is
/// authorized graph data sitting in a temporary directory after the authorization ended.
#[derive(Debug)]
pub struct Handover {
    directory: PathBuf,
    exchange: Exchange,
}

impl Handover {
    /// What to send in the request.
    #[must_use]
    pub fn exchange(&self) -> &Exchange {
        &self.exchange
    }
}

impl Drop for Handover {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Renders a graph as the media type version 1 defines, and writes it where a plugin can read it.
///
/// The document is versioned, because the media type evolves separately from the protocol carrying
/// it and a reader has to know which one it is holding.
///
/// A plugin never receives `.nostdb` and never a path into the database. It receives this, and the
/// digest it can check the bytes against.
///
/// # Errors
///
/// Returns a reason when the artifact cannot be written.
pub fn hand_over(graph: &nostdb_core::encoding::Graph, label: &str) -> Result<Handover, String> {
    let nodes: Vec<serde_json::Value> = graph
        .nodes
        .iter()
        .map(|node| {
            serde_json::json!({
                "id": node.id.to_string(),
                "labels": node.labels.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "properties": properties(&node.properties),
            })
        })
        .collect();

    let edges: Vec<serde_json::Value> = graph
        .edges
        .iter()
        .map(|edge| {
            serde_json::json!({
                "id": edge.id.to_string(),
                "relation": edge.relation.to_string(),
                // Scoped, so a plugin can tell a local endpoint from one in a linked source
                // without inventing a synthetic relationship between them.
                "source": endpoint(&edge.source),
                "target": endpoint(&edge.target),
                "properties": properties(&edge.properties),
            })
        })
        .collect();

    let document = serde_json::json!({
        // Version 2: a property value may be an object, and a list holds values rather than
        // scalars, so a list of objects and a list of lists both appear here. A version 1
        // reader expecting a scalar in either position would misread one, which is what the
        // field is for.
        "graph_exchange_version": 2,
        "nodes": nodes,
        "edges": edges,
    });
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("the graph could not be rendered: {error}"))?;

    let mut directory = std::env::temp_dir();
    directory.push(format!("nostdb-exchange-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let path = directory.join("graph.json");
    std::fs::write(&path, &bytes).map_err(|error| format!("{}: {error}", path.display()))?;

    Ok(Handover {
        exchange: Exchange {
            media_type: GRAPH_MEDIA_TYPE.to_owned(),
            path,
            bytes: bytes.len() as u64,
            content_digest: digest_bytes(&bytes).to_string(),
        },
        directory,
    })
}

/// Renders a property list, which is a rendering and not an interpretation.
fn properties(
    pairs: &[(
        nostdb_core::name::PropertyKey,
        nostdb_core::property::PropertyValue,
    )],
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    for (key, value) in pairs {
        object.insert(key.to_string(), property(value));
    }
    serde_json::Value::Object(object)
}

fn property(value: &nostdb_core::property::PropertyValue) -> serde_json::Value {
    use nostdb_core::property::PropertyValue;
    match value {
        PropertyValue::Boolean(flag) => serde_json::Value::from(*flag),
        PropertyValue::Integer(number) => serde_json::Value::from(*number),
        PropertyValue::Float(number) => serde_json::Value::from(number.get()),
        PropertyValue::String(text) => serde_json::Value::from(text.as_str()),
        // Rendered rather than embedded raw: JSON has no byte string, and a lossy conversion would
        // hand a plugin something that looked like text and was not.
        PropertyValue::Bytes(bytes) => serde_json::json!({"bytes": bytes.len()}),
        PropertyValue::DateTime(when) => serde_json::Value::from(when.to_string()),
        // Both containers recurse, because a list element is a value and a value may be an object.
        // The list arm used to duplicate every scalar case, which is what a list of scalars needed
        // and a list of objects cannot use.
        PropertyValue::List(many) => serde_json::Value::Array(many.iter().map(property).collect()),
        // Emitted as a plain JSON object rather than tagged. The only tag a value position in this
        // document carries is `{"bytes": n}`, and `bytes` is a reserved word in `.nost`, so no
        // property key can ever be one — a bare object here is unambiguous. The result envelope
        // tags its object form because `relationship`, `path`, and `object` are *not* reserved and
        // a stored key may use them.
        PropertyValue::Map(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, held)| (key.to_string(), property(held)))
                .collect(),
        ),
    }
}

fn endpoint(reference: &nostdb_core::graph::NodeReference) -> serde_json::Value {
    use nostdb_core::graph::NodeReference;
    match reference {
        NodeReference::Local(id) => serde_json::json!({"node": id.to_string()}),
        // The source travels with the identifier, because a record is identified across databases
        // by the pair and an identifier alone would collide.
        NodeReference::External(scoped) => serde_json::json!({
            "node": scoped.local.to_string(),
            "source": scoped.source.as_str(),
        }),
    }
}
