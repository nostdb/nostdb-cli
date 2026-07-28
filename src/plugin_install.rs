//! Installing a plugin: fetching, digests, consent, and the record.
//!
//! [`crate::plugin`] decides whether a plugin is acceptable and has no way to obtain one. This
//! obtains one, records what was approved, and still executes nothing.
//!
//! # Nothing here runs a plugin
//!
//! There is no `Command`, no `spawn`, and no path from this module to one. That is the
//! contract's separation rather than a convenience: installation MUST NOT execute plugin code,
//! and the surest way to hold that is for the installing code to have no way to run anything.
//!
//! # Retrieval goes through the provider
//!
//! Every byte arrives through [`nostdb_core::provider::ProviderClient`], which speaks the
//! published provider protocol to a separate process. The command surface must not bundle a
//! GitHub implementation, and the protocol already has exactly the three requests an install
//! needs: resolve a ref to a commit, enumerate a tree, read an entry.
//!
//! It is also what makes this testable. `Transport` is a trait, so every test here drives a
//! scripted conversation and no test reaches the network.

use crate::exit::ExitClass;
use crate::plugin::{PluginSource, validate_manifest};
use nostdb_core::provider::{Entry, ProviderClient, ProviderError, Transport};
use nostdb_core::sync::digest_bytes;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file every plugin contains.
pub const MANIFEST_NAME: &str = "nostdb-plugin.json";

/// The record versions this build reads.
pub const SUPPORTED_RECORD_VERSIONS: [u64; 1] = [1];

/// Entries in one plugin.
pub const MAX_ENTRIES: usize = 4096;
/// Bytes in one entry.
pub const MAX_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
/// Bytes in one plugin.
pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// Bytes in one path.
pub const MAX_PATH_BYTES: usize = 1024;
/// Segments in one path.
pub const MAX_PATH_DEPTH: usize = 32;

/// Names Windows reserves whatever the extension.
const RESERVED_STEMS: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Why an installation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InstallCode {
    /// A fetched tree is not a plugin.
    SourceInvalid,
    /// A tree exceeds a fixed archive limit.
    LimitExceeded,
    /// The manifest's range parses and excludes this build.
    Incompatible,
    /// A recorded commit yielded different bytes than the record says.
    DigestMismatch,
    /// The record breaks a rule decidable by reading it.
    RecordInvalid,
    /// The `plugin_install_version` is not one this build reads.
    RecordVersionUnsupported,
}

impl InstallCode {
    /// Every code, so a test can walk them.
    pub const ALL: [Self; 6] = [
        Self::SourceInvalid,
        Self::LimitExceeded,
        Self::Incompatible,
        Self::DigestMismatch,
        Self::RecordInvalid,
        Self::RecordVersionUnsupported,
    ];

    /// The symbolic name a refusal carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceInvalid => "PLUGIN_SOURCE_INVALID",
            Self::LimitExceeded => "PLUGIN_LIMIT_EXCEEDED",
            Self::Incompatible => "PLUGIN_INCOMPATIBLE",
            Self::DigestMismatch => "PLUGIN_DIGEST_MISMATCH",
            Self::RecordInvalid => "PLUGIN_RECORD_INVALID",
            Self::RecordVersionUnsupported => "PLUGIN_RECORD_VERSION_UNSUPPORTED",
        }
    }

    /// The exit class a refusal reports.
    ///
    /// A document that will not do is a validation failure, which is how a refused manifest is
    /// already reported: a tree, a record, and a manifest are all documents, and the fix is to
    /// correct or replace one.
    ///
    /// The two that are not about a document are class 7. An incompatible plugin is correct and
    /// unusable here, and a digest mismatch is a plugin that is not what was approved — neither
    /// is a document somebody edits, and both are what class 7 exists for.
    #[must_use]
    pub const fn exit_class(self) -> ExitClass {
        match self {
            Self::SourceInvalid
            | Self::LimitExceeded
            | Self::RecordInvalid
            | Self::RecordVersionUnsupported => ExitClass::Validation,
            Self::Incompatible | Self::DigestMismatch => ExitClass::Plugin,
        }
    }
}

impl std::fmt::Display for InstallCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A refusal, with the reason a person reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallError {
    /// The code a script branches on.
    pub code: InstallCode,
    /// Why, for a person.
    pub reason: String,
}

impl InstallError {
    fn new(code: InstallCode, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.reason)
    }
}

impl std::error::Error for InstallError {}

/// A three-component version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// Parses `major.minor.patch`.
    ///
    /// # Errors
    ///
    /// Returns a reason. A leading zero is refused, because a number with two spellings gives
    /// one range two spellings and two digests.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut parts = text.split('.');
        let mut component = || -> Result<u64, String> {
            let raw = parts
                .next()
                .ok_or_else(|| format!("`{text}` is not major.minor.patch"))?;
            if raw.is_empty() {
                return Err(format!("`{text}` has an empty component"));
            }
            if raw.len() > 1 && raw.starts_with('0') {
                return Err(format!("`{raw}` has a leading zero"));
            }
            raw.parse::<u64>()
                .map_err(|_| format!("`{raw}` is not a number"))
        };
        let major = component()?;
        let minor = component()?;
        let patch = component()?;
        if parts.next().is_some() {
            return Err(format!("`{text}` has more than three components"));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One comparator's relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    AtLeast,
    Above,
    AtMost,
    Below,
    Exactly,
}

/// The Engine versions a plugin declares it works with.
///
/// A conjunction of comparators, and deliberately nothing more. A caret or a tilde means
/// different things in different ecosystems, so an author who wrote one expecting one reading
/// would get another; a conjunction has one reading everywhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineRange {
    comparators: Vec<(Operator, Version)>,
}

impl EngineRange {
    /// Parses a range.
    ///
    /// # Errors
    ///
    /// Returns a reason. The caller reports it as a refused manifest member: a range that does
    /// not parse is a malformed manifest, not an incompatibility.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut comparators = Vec::new();
        for token in text.split_whitespace() {
            let (operator, rest) = if let Some(rest) = token.strip_prefix(">=") {
                (Operator::AtLeast, rest)
            } else if let Some(rest) = token.strip_prefix("<=") {
                (Operator::AtMost, rest)
            } else if let Some(rest) = token.strip_prefix('>') {
                (Operator::Above, rest)
            } else if let Some(rest) = token.strip_prefix('<') {
                (Operator::Below, rest)
            } else if let Some(rest) = token.strip_prefix('=') {
                (Operator::Exactly, rest)
            } else {
                // A bare version is not a comparator. Whether it would mean `=` or `>=` is
                // exactly the ambiguity naming the operator removes.
                return Err(format!("`{token}` states no comparator"));
            };
            comparators.push((operator, Version::parse(rest)?));
        }
        if comparators.is_empty() {
            return Err("a range states at least one comparator".to_owned());
        }
        Ok(Self { comparators })
    }

    /// Reports whether every comparator holds for this version.
    #[must_use]
    pub fn admits(&self, version: &Version) -> bool {
        self.comparators
            .iter()
            .all(|(operator, bound)| match operator {
                Operator::AtLeast => version >= bound,
                Operator::Above => version > bound,
                Operator::AtMost => version <= bound,
                Operator::Below => version < bound,
                Operator::Exactly => version == bound,
            })
    }
}

/// Where an installation lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Under the project, and taking precedence over a global installation of the same name.
    Project,
    /// Under the user's home directory.
    Global,
}

impl Scope {
    /// The name the record carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Global => "global",
        }
    }

    /// Reads a scope from its recorded name.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "project" => Some(Self::Project),
            "global" => Some(Self::Global),
            _ => None,
        }
    }
}

/// One entry that is part of the plugin, with its path already narrowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanEntry {
    /// The path within the plugin.
    pub path: String,
    /// The path within the snapshot, which is what a read asks for.
    pub source_path: String,
    /// The size the enumeration reported.
    pub bytes: u64,
}

/// Decides which entries are part of the plugin, and refuses a tree that is not one.
///
/// Every rule here is decidable from an enumeration, which is the property that lets a tree be
/// refused before anything is downloaded. A limit checked after the bytes arrive is a
/// description of what was downloaded rather than a limit on it.
///
/// # Errors
///
/// Returns [`InstallCode::SourceInvalid`] for a tree that is not a plugin, and
/// [`InstallCode::LimitExceeded`] for one over a fixed bound.
pub fn plan(entries: &[Entry], subdirectory: Option<&str>) -> Result<Vec<PlanEntry>, InstallError> {
    let prefix = subdirectory.map(|sub| format!("{}/", sub.trim_end_matches('/')));

    let mut planned: Vec<PlanEntry> = Vec::new();
    for entry in entries {
        let within = match &prefix {
            // Outside the subdirectory is not part of the plugin, so it is not validated and
            // not digested. It is not a file that was rejected; it is a file that was not
            // installed.
            Some(prefix) => match entry.path.strip_prefix(prefix.as_str()) {
                Some(rest) => rest,
                None => continue,
            },
            None => entry.path.as_str(),
        };
        check_path(within)?;
        planned.push(PlanEntry {
            path: within.to_owned(),
            source_path: entry.path.clone(),
            bytes: entry.bytes,
        });
    }

    if planned.is_empty() {
        return Err(InstallError::new(
            InstallCode::SourceInvalid,
            match subdirectory {
                Some(sub) => format!("`{sub}` names nothing in this snapshot"),
                None => "the snapshot holds nothing".to_owned(),
            },
        ));
    }

    // The manifest is at the plugin's root, not searched for. A manager that searched would be
    // choosing which of two candidates is the plugin.
    if !planned.iter().any(|entry| entry.path == MANIFEST_NAME) {
        return Err(InstallError::new(
            InstallCode::SourceInvalid,
            format!("there is no {MANIFEST_NAME} at the plugin's root"),
        ));
    }

    for entry in &planned {
        if entry.path.len() > MAX_PATH_BYTES {
            return Err(InstallError::new(
                InstallCode::LimitExceeded,
                format!(
                    "`{}` is {} bytes, over the {MAX_PATH_BYTES}-byte path limit",
                    entry.path,
                    entry.path.len()
                ),
            ));
        }
        let depth = entry.path.split('/').count();
        if depth > MAX_PATH_DEPTH {
            return Err(InstallError::new(
                InstallCode::LimitExceeded,
                format!(
                    "`{}` is {depth} segments deep, over {MAX_PATH_DEPTH}",
                    entry.path
                ),
            ));
        }
        if entry.bytes > MAX_ENTRY_BYTES {
            return Err(InstallError::new(
                InstallCode::LimitExceeded,
                format!(
                    "`{}` is {} bytes, over the {MAX_ENTRY_BYTES}-byte entry limit",
                    entry.path, entry.bytes
                ),
            ));
        }
    }

    if planned.len() > MAX_ENTRIES {
        return Err(InstallError::new(
            InstallCode::LimitExceeded,
            format!("{} entries, over {MAX_ENTRIES}", planned.len()),
        ));
    }

    let total: u64 = planned.iter().map(|entry| entry.bytes).sum();
    if total > MAX_TOTAL_BYTES {
        return Err(InstallError::new(
            InstallCode::LimitExceeded,
            format!("{total} bytes, over the {MAX_TOTAL_BYTES}-byte plugin limit"),
        ));
    }

    // Ascending byte order, because the tree digest is taken in that order and a digest that
    // depended on the order a host happened to enumerate in would differ between two machines
    // installing the same commit.
    planned.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    Ok(planned)
}

/// Refuses a path that names something outside the plugin, or that does not round-trip.
fn check_path(path: &str) -> Result<(), InstallError> {
    let invalid = |reason: String| InstallError::new(InstallCode::SourceInvalid, reason);

    if path.is_empty() {
        return Err(invalid("an entry has an empty path".to_owned()));
    }
    if path.starts_with('/') {
        return Err(invalid(format!("`{path}` is absolute")));
    }
    // A Windows drive or UNC path is absolute on the platform that reads it that way, and an
    // install must not be a different act depending on where it runs.
    let bytes = path.as_bytes();
    if path.starts_with('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err(invalid(format!("`{path}` is an absolute Windows path")));
    }
    if path.contains('\\') {
        return Err(invalid(format!(
            "`{path}` contains a backslash, which is a separator on one supported platform and a name character on another"
        )));
    }
    if path.chars().any(|c| c < '\u{20}' || c == '\u{7f}') {
        return Err(invalid(
            "an entry path contains a control character".to_owned(),
        ));
    }

    for segment in path.split('/') {
        if segment.is_empty() {
            return Err(invalid(format!("`{path}` has an empty segment")));
        }
        if segment == "." || segment == ".." {
            return Err(invalid(format!(
                "`{path}` names something outside the plugin"
            )));
        }
        if segment.ends_with(' ') || segment.ends_with('.') {
            return Err(invalid(format!(
                "`{segment}` ends in a space or a period, which does not round-trip on every supported filesystem"
            )));
        }
        let stem = segment.split('.').next().unwrap_or(segment);
        if RESERVED_STEMS
            .iter()
            .any(|reserved| stem.eq_ignore_ascii_case(reserved))
        {
            return Err(invalid(format!(
                "`{segment}` is a reserved device name on one supported platform"
            )));
        }
    }
    Ok(())
}

/// The digest of the entries an installation accepted.
///
/// `<path> LF <hex> LF` per entry, in ascending byte order of path. Written out in the contract
/// so a second implementation can reproduce it, which is the only thing that makes recording a
/// digest worth doing.
#[must_use]
pub fn tree_digest(entries: &[(String, Vec<u8>)]) -> String {
    let mut ordered: Vec<&(String, Vec<u8>)> = entries.iter().collect();
    ordered.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));

    let mut material: Vec<u8> = Vec::new();
    for (path, bytes) in ordered {
        material.extend_from_slice(path.as_bytes());
        material.push(b'\n');
        material.extend_from_slice(digest_bytes(bytes).hex().as_bytes());
        material.push(b'\n');
    }
    digest_bytes(&material).to_string()
}

/// One installation, as the record carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installation {
    /// The manifest's `name`, and the key a project pins.
    pub name: String,
    /// The canonical repository URL, with no ref and no fragment.
    pub repository: String,
    /// The commit the ref resolved to.
    pub commit: String,
    /// The plugin's directory within the repository.
    pub subdirectory: Option<String>,
    /// The digest of the manifest's bytes as received.
    pub manifest_digest: String,
    /// The digest of the accepted entries.
    pub tree_digest: String,
    /// Where this is installed.
    pub scope: Scope,
    /// The manifest version this plugin declared.
    pub manifest_version: u64,
    /// The manifest's `version`.
    pub plugin_version: String,
    /// The manifest's `permissions`, as approved.
    pub approved_permissions: serde_json::Value,
}

fn is_a_digest(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

fn is_a_commit(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Every installation in one scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    installed: Vec<Installation>,
}

impl Record {
    /// What is installed, ordered by name.
    #[must_use]
    pub fn installed(&self) -> &[Installation] {
        &self.installed
    }

    /// Finds an installation by plugin name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Installation> {
        self.installed.iter().find(|entry| entry.name == name)
    }

    /// Adds or replaces an installation, keeping the file canonical.
    pub fn upsert(&mut self, installation: Installation) {
        self.installed
            .retain(|entry| entry.name != installation.name);
        self.installed.push(installation);
        self.installed
            .sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    }

    /// Reads a record, reporting every problem found rather than the first.
    ///
    /// Not repaired, for the reason a malformed manifest is not: this file is the evidence of
    /// what a user approved, and rewriting it into something acceptable would be deciding on
    /// their behalf what they had agreed to.
    ///
    /// # Errors
    ///
    /// Returns the code and the problems. An unsupported version is returned alone, because
    /// nothing after an unreadable version is interpretable.
    pub fn parse(text: &str, scope: Scope) -> Result<Self, (InstallCode, Vec<String>)> {
        let invalid = |reasons: Vec<String>| (InstallCode::RecordInvalid, reasons);
        let document: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| invalid(vec![format!("the record is not JSON: {error}")]))?;
        let Some(root) = document.as_object() else {
            return Err(invalid(vec!["the record is not an object".to_owned()]));
        };

        match root
            .get("plugin_install_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(found) if SUPPORTED_RECORD_VERSIONS.contains(&found) => {}
            Some(found) => {
                return Err((
                    InstallCode::RecordVersionUnsupported,
                    vec![format!(
                        "plugin_install_version {found} is not one this build reads"
                    )],
                ));
            }
            None => {
                return Err(invalid(vec!["plugin_install_version is absent".to_owned()]));
            }
        }

        let Some(entries) = root.get("installed").and_then(serde_json::Value::as_array) else {
            return Err(invalid(vec![
                "installed is absent or is not an array".to_owned(),
            ]));
        };

        let mut problems = Vec::new();
        let mut installed = Vec::new();
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();
        let mut previous: Option<String> = None;

        for entry in entries {
            let Some(entry) = entry.as_object() else {
                problems.push("an entry is not an object".to_owned());
                continue;
            };

            let mut text_member =
                |key: &str| match entry.get(key).and_then(serde_json::Value::as_str) {
                    Some(value) => Some(value.to_owned()),
                    None => {
                        problems.push(format!("an entry has no {key}"));
                        None
                    }
                };

            let name = text_member("name");
            let repository = text_member("repository");
            let commit = text_member("commit");
            let manifest_digest = text_member("manifest_digest");
            let tree_digest = text_member("tree_digest");
            let scope_text = text_member("scope");
            let plugin_version = text_member("plugin_version");

            let manifest_version = entry
                .get("manifest_version")
                .and_then(serde_json::Value::as_u64);
            if manifest_version.is_none() {
                problems.push("an entry has no manifest_version".to_owned());
            }
            let permissions = entry.get("approved_permissions").cloned();
            if permissions
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .is_none()
            {
                problems.push("an entry has no approved_permissions object".to_owned());
            }

            if let Some(name) = &name {
                if seen.insert(name.clone(), ()).is_some() {
                    problems.push(format!("two entries share the name {name}"));
                }
                if let Some(previous) = &previous {
                    if previous.as_bytes() >= name.as_bytes() {
                        problems.push(format!("{name} follows {previous}, which is not ascending"));
                    }
                }
                previous = Some(name.clone());
            }

            for (key, value) in [
                ("manifest_digest", &manifest_digest),
                ("tree_digest", &tree_digest),
            ] {
                if let Some(value) = value {
                    if !is_a_digest(value) {
                        problems.push(format!(
                            "{key} is {value}, which is not sha256 and 64 lower-case hex"
                        ));
                    }
                }
            }
            if let Some(commit) = &commit {
                if !is_a_commit(commit) {
                    problems.push(format!(
                        "commit is {commit}, which is not 40 lower-case hex characters"
                    ));
                }
            }
            let parsed_scope = scope_text.as_deref().and_then(Scope::parse);
            if let Some(text) = &scope_text {
                match parsed_scope {
                    None => problems.push(format!("scope is {text}")),
                    Some(found) if found != scope => problems.push(format!(
                        "scope is {text} in a {} record, which would claim the other scope's precedence",
                        scope.as_str()
                    )),
                    Some(_) => {}
                }
            }
            if permissions
                .as_ref()
                .and_then(|value| value.get("database_write"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                problems.push(
                    "an entry approves database_write; only the Engine writes .nostdb".to_owned(),
                );
            }

            if let (
                Some(name),
                Some(repository),
                Some(commit),
                Some(manifest_digest),
                Some(tree_digest),
                Some(scope_found),
                Some(manifest_version),
                Some(plugin_version),
                Some(permissions),
            ) = (
                name,
                repository,
                commit,
                manifest_digest,
                tree_digest,
                parsed_scope,
                manifest_version,
                plugin_version,
                permissions,
            ) {
                installed.push(Installation {
                    name,
                    repository,
                    commit,
                    subdirectory: entry
                        .get("subdirectory")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    manifest_digest,
                    tree_digest,
                    scope: scope_found,
                    manifest_version,
                    plugin_version,
                    approved_permissions: permissions,
                });
            }
        }

        if problems.is_empty() {
            Ok(Self { installed })
        } else {
            Err(invalid(problems))
        }
    }

    /// Renders the record.
    ///
    /// Ordered by name so two managers installing the same set in different orders produce the
    /// same file. A record that differed only by insertion order would show as a change in every
    /// diff and in every backup.
    #[must_use]
    pub fn to_json(&self) -> String {
        let entries: Vec<serde_json::Value> = self
            .installed
            .iter()
            .map(|entry| {
                let mut object = serde_json::Map::new();
                object.insert("name".to_owned(), entry.name.clone().into());
                object.insert("repository".to_owned(), entry.repository.clone().into());
                object.insert("commit".to_owned(), entry.commit.clone().into());
                if let Some(subdirectory) = &entry.subdirectory {
                    object.insert("subdirectory".to_owned(), subdirectory.clone().into());
                }
                object.insert(
                    "manifest_digest".to_owned(),
                    entry.manifest_digest.clone().into(),
                );
                object.insert("tree_digest".to_owned(), entry.tree_digest.clone().into());
                object.insert("scope".to_owned(), entry.scope.as_str().into());
                object.insert("manifest_version".to_owned(), entry.manifest_version.into());
                object.insert(
                    "plugin_version".to_owned(),
                    entry.plugin_version.clone().into(),
                );
                object.insert(
                    "approved_permissions".to_owned(),
                    entry.approved_permissions.clone(),
                );
                serde_json::Value::Object(object)
            })
            .collect();

        let document = serde_json::json!({
            "plugin_install_version": SUPPORTED_RECORD_VERSIONS[0],
            "installed": entries,
        });
        let mut text = serde_json::to_string_pretty(&document).unwrap_or_default();
        text.push('\n');
        text
    }
}

/// What a fetch produced, before anything is written.
#[derive(Clone, Debug)]
pub struct Fetched {
    /// The commit the ref resolved to.
    pub commit: String,
    /// The manifest's `name`.
    pub name: String,
    /// The manifest's `version`.
    pub plugin_version: String,
    /// The manifest version the plugin declared.
    pub manifest_version: u64,
    /// The manifest's `permissions`.
    pub permissions: serde_json::Value,
    /// The digest of the manifest's bytes as received.
    pub manifest_digest: String,
    /// The digest of every accepted entry.
    pub tree_digest: String,
    /// Every accepted entry, keyed by its path within the plugin.
    pub files: Vec<(String, Vec<u8>)>,
}

/// The locator a plugin source resolves through.
///
/// The provider's contract requires a `ref` and forbids inventing one, because a default branch
/// can change and a locator is an identity. A source that named no ref therefore cannot be
/// turned into a locator here — see the recorded conflict in the root progress document.
///
/// # Errors
///
/// Returns a reason when the source names no ref.
pub fn locator_for(source: &PluginSource) -> Result<String, InstallError> {
    let reference = source.reference().ok_or_else(|| {
        InstallError::new(
            InstallCode::SourceInvalid,
            "this source names no ref, and resolving a default branch is a published conflict \
             between the manifest contract and the provider protocol; name a ref with \
             `?ref=<git-ref>`",
        )
    })?;
    Ok(format!(
        "github://{}/{}/?ref={reference}",
        source.owner(),
        source.repository()
    ))
}

/// Fetches a plugin and decides whether it may be installed. Writes nothing.
///
/// # Errors
///
/// Returns an [`InstallError`] for a tree, manifest, or range this build refuses, and a
/// [`ProviderError`] for a conversation that failed. The provider's own code is passed through
/// rather than relabelled: if the host was unreachable, that is what happened, and calling it a
/// plugin failure would hide which layer failed.
pub fn fetch<T: Transport>(
    client: &mut ProviderClient<T>,
    source: &PluginSource,
    engine: &Version,
) -> Result<Fetched, FetchError> {
    let locator = locator_for(source)?;
    let snapshot = client.resolve(&locator, None)?;
    let entries = client.enumerate(&snapshot.snapshot)?;
    let planned = plan(&entries, source.subdirectory())?;

    // The manifest first, and on its own. A tree that is not a plugin is refused before its
    // bytes are paid for, and a manifest that will not do is refused before the rest arrives.
    let manifest_entry = planned
        .iter()
        .find(|entry| entry.path == MANIFEST_NAME)
        .ok_or_else(|| {
            InstallError::new(
                InstallCode::SourceInvalid,
                format!("there is no {MANIFEST_NAME} at the plugin's root"),
            )
        })?;
    let manifest_bytes = client.read(&snapshot.snapshot, &manifest_entry.source_path)?;
    let manifest_text = String::from_utf8(manifest_bytes.clone())
        .map_err(|_| InstallError::new(InstallCode::SourceInvalid, "the manifest is not UTF-8"))?;
    validate_manifest(&manifest_text)
        .map_err(|(code, problems)| FetchError::Manifest { code, problems })?;

    let manifest: serde_json::Value = serde_json::from_str(&manifest_text).map_err(|error| {
        InstallError::new(
            InstallCode::SourceInvalid,
            format!("the manifest is not JSON: {error}"),
        )
    })?;

    let declared = manifest["nostdb"].as_str().unwrap_or_default();
    let range = EngineRange::parse(declared).map_err(|reason| FetchError::Manifest {
        code: crate::plugin::PluginCode::ManifestInvalid,
        problems: vec![format!("nostdb is not a range: {reason}")],
    })?;
    if !range.admits(engine) {
        return Err(InstallError::new(
            InstallCode::Incompatible,
            format!("this plugin declares `{declared}` and this build is {engine}"),
        )
        .into());
    }

    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(planned.len());
    for entry in &planned {
        let bytes = if entry.path == MANIFEST_NAME {
            manifest_bytes.clone()
        } else {
            client.read(&snapshot.snapshot, &entry.source_path)?
        };
        files.push((entry.path.clone(), bytes));
    }

    Ok(Fetched {
        commit: snapshot.snapshot,
        name: manifest["name"].as_str().unwrap_or_default().to_owned(),
        plugin_version: manifest["version"].as_str().unwrap_or_default().to_owned(),
        manifest_version: manifest["manifest_version"].as_u64().unwrap_or_default(),
        permissions: manifest["permissions"].clone(),
        manifest_digest: digest_bytes(&manifest_bytes).to_string(),
        tree_digest: tree_digest(&files),
        files,
    })
}

/// Why a fetch did not produce an installable plugin.
#[derive(Clone, Debug)]
pub enum FetchError {
    /// The installation contract refused it.
    Install(InstallError),
    /// The manifest contract refused it.
    Manifest {
        /// The manifest code.
        code: crate::plugin::PluginCode,
        /// Every problem found, rather than the first.
        problems: Vec<String>,
    },
    /// The provider conversation failed. The provider's own code is reported.
    Provider(ProviderError),
}

impl From<InstallError> for FetchError {
    fn from(error: InstallError) -> Self {
        Self::Install(error)
    }
}

impl From<ProviderError> for FetchError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Install(error) => write!(formatter, "{error}"),
            Self::Manifest { code, problems } => {
                write!(formatter, "{code}: {}", problems.join("; "))
            }
            Self::Provider(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for FetchError {}

impl FetchError {
    /// The exit class this refusal reports.
    #[must_use]
    pub fn exit_class(&self) -> ExitClass {
        match self {
            Self::Install(error) => error.code.exit_class(),
            Self::Manifest { code, .. } => code.exit_class(),
            // A host that could not be reached is unavailable, which is what class 5 is for.
            // Relabelling it as a plugin failure would say the plugin was the problem.
            Self::Provider(_) => ExitClass::Unavailable,
        }
    }

    /// The symbolic code this refusal carries.
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::Install(error) => error.code.to_string(),
            Self::Manifest { code, .. } => code.to_string(),
            Self::Provider(error) => provider_code(error),
        }
    }
}

/// The code a provider failure reports.
fn provider_code(error: &ProviderError) -> String {
    match error {
        ProviderError::Refused { code, .. } => code.clone(),
        // A provider that broke the protocol is a defect in the provider, and the registry has
        // a code for exactly that rather than for the host being down.
        ProviderError::VersionMismatch { .. } => "PROVIDER_PROTOCOL_UNSUPPORTED".to_owned(),
        _ => "PROVIDER_SOURCE_UNAVAILABLE".to_owned(),
    }
}

/// What committing an installation did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The plugin and the record were written.
    Installed,
    /// The same commit and the same digests were already recorded, so nothing was written.
    AlreadyInstalled,
    /// A different commit replaced what was recorded.
    Replaced,
}

/// Where a scope's files and record live.
#[must_use]
pub fn plugins_directory(root: &Path) -> PathBuf {
    root.join(".nostdb").join("plugins")
}

/// The record path for a scope rooted at `root`.
#[must_use]
pub fn record_path(root: &Path) -> PathBuf {
    plugins_directory(root).join("installed.json")
}

/// Reads the record for a scope, or an empty one when none exists.
///
/// # Errors
///
/// Returns an [`InstallError`] for a record that will not do, and for one that cannot be read.
pub fn read_record(root: &Path, scope: Scope) -> Result<Record, InstallError> {
    let path = record_path(root);
    match std::fs::read_to_string(&path) {
        Ok(text) => Record::parse(&text, scope).map_err(|(code, problems)| {
            InstallError::new(code, format!("{}: {}", path.display(), problems.join("; ")))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
        Err(error) => Err(InstallError::new(
            InstallCode::RecordInvalid,
            format!("cannot read {}: {error}", path.display()),
        )),
    }
}

/// Writes the plugin's files and the record.
///
/// # Errors
///
/// Returns [`InstallCode::DigestMismatch`] when the recorded commit yielded different bytes, and
/// an I/O reason when anything cannot be written. A failure leaves the previous record in place:
/// the record is promoted last, so a partially written plugin is never one the record names.
pub fn commit_install(
    fetched: &Fetched,
    source: &PluginSource,
    root: &Path,
    scope: Scope,
) -> Result<Outcome, InstallError> {
    let mut record = read_record(root, scope)?;

    let mut outcome = Outcome::Installed;
    if let Some(existing) = record.find(&fetched.name) {
        if existing.commit == fetched.commit {
            let same = existing.manifest_digest == fetched.manifest_digest
                && existing.tree_digest == fetched.tree_digest;
            if same {
                return Ok(Outcome::AlreadyInstalled);
            }
            // A commit is immutable, so the same commit yielding different bytes means
            // something between the host and this machine is not what it was. Replacing the
            // record would overwrite the only evidence that anything had changed.
            let which = if existing.manifest_digest == fetched.manifest_digest {
                "the code changed behind an unchanged manifest"
            } else {
                "the manifest changed"
            };
            return Err(InstallError::new(
                InstallCode::DigestMismatch,
                format!(
                    "{} is recorded at {} and {which}",
                    fetched.name, existing.commit
                ),
            ));
        }
        outcome = Outcome::Replaced;
    }

    let directory = plugins_directory(root).join(&fetched.name);
    let staging = plugins_directory(root).join(format!(".{}.staging", fetched.name));
    let io = |what: &str, error: &std::io::Error| {
        InstallError::new(
            InstallCode::RecordInvalid,
            format!("cannot {what}: {error}"),
        )
    };

    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|error| io("create the staging directory", &error))?;
    for (path, bytes) in &fetched.files {
        let target = staging.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| io("create a directory", &error))?;
        }
        std::fs::write(&target, bytes).map_err(|error| io("write a plugin file", &error))?;
    }

    // Promoted only once every file is written, so an interrupted install leaves the previous
    // plugin in place rather than half of the new one.
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::rename(&staging, &directory).map_err(|error| {
        let _ = std::fs::remove_dir_all(&staging);
        io("promote the plugin directory", &error)
    })?;

    record.upsert(Installation {
        name: fetched.name.clone(),
        repository: format!(
            "https://github.com/{}/{}",
            source.owner(),
            source.repository()
        ),
        commit: fetched.commit.clone(),
        subdirectory: source.subdirectory().map(str::to_owned),
        manifest_digest: fetched.manifest_digest.clone(),
        tree_digest: fetched.tree_digest.clone(),
        scope,
        manifest_version: fetched.manifest_version,
        plugin_version: fetched.plugin_version.clone(),
        approved_permissions: fetched.permissions.clone(),
    });

    let path = record_path(root);
    let staged = path.with_extension("json.staging");
    std::fs::write(&staged, record.to_json()).map_err(|error| io("write the record", &error))?;
    std::fs::rename(&staged, &path).map_err(|error| {
        let _ = std::fs::remove_file(&staged);
        io("promote the record", &error)
    })?;

    Ok(outcome)
}

/// The Engine version this build reports.
///
/// # Errors
///
/// Returns a reason when the crate version is not three components, which would be a defect in
/// this build rather than in a plugin.
pub fn engine_version() -> Result<Version, String> {
    Version::parse(crate::VERSION)
}

/// A parsed `plugin` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Install a plugin from a GitHub source.
    Add {
        /// The source, as written.
        source: String,
        /// The scope, when the invocation named one.
        scope: Option<Scope>,
    },
}

/// The actions this build implements.
pub const IMPLEMENTED: [&str; 1] = ["add"];

/// The actions the product contract names and this build does not implement.
const AWAITING_EXECUTION: [&str; 2] = ["list", "remove"];

/// Parses `plugin ...`, or reports the usage that was broken.
///
/// # Errors
///
/// Returns the usage message for a malformed invocation, which the caller reports as exit class
/// 2. An action the product contract names and this build does not implement is refused by name
/// rather than as unknown: somebody who typed a real command deserves to be told it is not built
/// yet, not that it does not exist.
pub fn parse(arguments: &[&str]) -> Result<Action, String> {
    let Some((action, rest)) = arguments.split_first() else {
        return Err(format!(
            "`plugin` needs an action; expected one of {IMPLEMENTED:?}"
        ));
    };

    if AWAITING_EXECUTION.contains(action) {
        return Err(format!(
            "`plugin {action}` reads the installation record, and nothing yet executes an \
             installed plugin, so a record this build can only write has nothing to list against"
        ));
    }
    if *action != "add" {
        return Err(format!(
            "`{action}` is not a plugin action; expected one of {IMPLEMENTED:?}"
        ));
    }

    let mut source: Option<String> = None;
    let mut scope: Option<Scope> = None;
    let mut index = 0;
    while index < rest.len() {
        let argument = rest[index];
        let (name, inline) = argument
            .split_once('=')
            .map_or((argument, None), |(name, value)| (name, Some(value)));
        match name {
            // `--scope` takes a value, and `--project` elsewhere on the surface takes a path.
            // Spelling the scope as `--project` would have made one word mean a scope here and
            // a directory everywhere else.
            "--scope" => {
                let value = match inline {
                    Some(value) => {
                        index += 1;
                        value
                    }
                    None => {
                        let Some(value) = rest.get(index + 1) else {
                            return Err("`--scope` needs a value: project or global".to_owned());
                        };
                        index += 2;
                        value
                    }
                };
                let chosen = Scope::parse(value).ok_or_else(|| {
                    format!("`{value}` is not a scope; expected project or global")
                })?;
                if scope.replace(chosen).is_some() {
                    return Err("`plugin add` takes one scope".to_owned());
                }
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`"));
            }
            other => {
                index += 1;
                if source.replace(other.to_owned()).is_some() {
                    return Err(format!("`plugin add` takes one source, found `{other}`"));
                }
            }
        }
    }

    let source = source.ok_or_else(|| {
        "`plugin add` needs a source: `nostdb plugin add 'https://github.com/owner/repository'`"
            .to_owned()
    })?;
    Ok(Action::Add { source, scope })
}

/// Where the provider executable is named.
///
/// The same variable `link` uses, because it is the same provider. A second name would let one
/// command reach a provider the other could not.
pub use crate::link::PROVIDER_VARIABLE;

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Decides where an installation goes.
///
/// An explicit `plugin add` is authorization to install. What is asked is *where*, and only when
/// the invocation did not say and somebody can answer.
///
/// A non-interactive session takes project scope rather than refusing for want of an answer:
/// refusing would make every unattended install depend on a person being present, and the
/// narrower of the two scopes is the safe one to choose without being told.
fn choose_scope(
    given: Option<Scope>,
    project: Option<&Path>,
    interactive: bool,
    input: &mut dyn std::io::BufRead,
    out: &mut dyn std::io::Write,
) -> Scope {
    if let Some(scope) = given {
        return scope;
    }
    if project.is_none() {
        return Scope::Global;
    }
    if !interactive {
        return Scope::Project;
    }

    let _ = writeln!(out, "Install for this project, or for every project?");
    let _ = write!(out, "  [P]roject (recommended) / [g]lobal: ");
    let _ = out.flush();
    let mut answer = String::new();
    match input.read_line(&mut answer) {
        Ok(0) | Err(_) => Scope::Project,
        Ok(_) => match answer.trim().to_ascii_lowercase().as_str() {
            "g" | "global" => Scope::Global,
            _ => Scope::Project,
        },
    }
}

/// Runs a `plugin` action.
///
/// `input` is read only to answer the scope question, and only when `interactive`. Taking it as a
/// reader rather than reaching for the terminal is what lets a test drive the whole exchange.
pub fn run(
    action: &Action,
    from: &Path,
    interactive: bool,
    input: &mut dyn std::io::BufRead,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> ExitClass {
    let Action::Add { source, scope } = action;

    let parsed = match PluginSource::parse(source) {
        Ok(parsed) => parsed,
        Err(reason) => {
            let _ = writeln!(err, "{}: {reason}", InstallCode::SourceInvalid);
            return InstallCode::SourceInvalid.exit_class();
        }
    };

    let engine = match engine_version() {
        Ok(version) => version,
        Err(reason) => {
            let _ = writeln!(err, "this build reports no readable version: {reason}");
            return ExitClass::Internal;
        }
    };

    let project = nostdb_core::project::Project::is_configured(from).then(|| from.to_owned());
    let scope = choose_scope(*scope, project.as_deref(), interactive, input, out);
    let root = match scope {
        Scope::Project => match &project {
            Some(root) => root.clone(),
            None => {
                let _ = writeln!(
                    err,
                    "{} is not a configured project, so there is nowhere to install for it",
                    from.display()
                );
                return ExitClass::Usage;
            }
        },
        Scope::Global => match home_directory() {
            Some(home) => home,
            None => {
                let _ = writeln!(err, "no home directory is set, so there is no global scope");
                return ExitClass::Usage;
            }
        },
    };

    // Whether this source can become a locator at all is decidable here, and it is checked before
    // a provider is demanded. Told to install a provider first, somebody would install one and
    // then meet this refusal anyway.
    if let Err(error) = locator_for(&parsed) {
        let _ = writeln!(err, "{}: {}", error.code, error.reason);
        return error.code.exit_class();
    }

    let Some(program) = std::env::var_os(PROVIDER_VARIABLE).map(PathBuf::from) else {
        let _ = writeln!(
            err,
            "{PROVIDER_VARIABLE} names no provider executable, and a plugin source needs one"
        );
        return ExitClass::Unavailable;
    };

    let process = match nostdb_core::provider_process::ProviderProcess::start(&program, &[]) {
        Ok(process) => process,
        Err(reason) => {
            let _ = writeln!(err, "{reason}");
            return ExitClass::Unavailable;
        }
    };
    let mut client = ProviderClient::new(process);
    if let Err(error) = client.handshake() {
        let _ = writeln!(err, "{}: {error}", provider_code(&error));
        return ExitClass::Unavailable;
    }

    let fetched = match fetch(&mut client, &parsed, &engine) {
        Ok(fetched) => fetched,
        Err(error) => {
            let _ = writeln!(err, "{}: {error}", error.code());
            return error.exit_class();
        }
    };

    // Shown before the record is written, because what a user approved is what the record says
    // they approved, and a permission nobody was shown is one nobody agreed to.
    let _ = writeln!(
        out,
        "{} {} at {}",
        fetched.name, fetched.plugin_version, fetched.commit
    );
    let _ = writeln!(out, "  permissions: {}", fetched.permissions);
    let _ = writeln!(out, "  manifest: {}", fetched.manifest_digest);
    let _ = writeln!(out, "  tree:     {}", fetched.tree_digest);

    match commit_install(&fetched, &parsed, &root, scope) {
        Ok(outcome) => {
            let where_to = plugins_directory(&root).join(&fetched.name);
            let _ = match outcome {
                Outcome::Installed => writeln!(
                    out,
                    "installed {} for {} in {}",
                    fetched.name,
                    scope.as_str(),
                    where_to.display()
                ),
                Outcome::Replaced => writeln!(
                    out,
                    "replaced {} for {} in {}",
                    fetched.name,
                    scope.as_str(),
                    where_to.display()
                ),
                Outcome::AlreadyInstalled => writeln!(
                    out,
                    "{} is already installed for {} at {}",
                    fetched.name,
                    scope.as_str(),
                    fetched.commit
                ),
            };
            ExitClass::Success
        }
        Err(error) => {
            let _ = writeln!(err, "{}: {}", error.code, error.reason);
            error.code.exit_class()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, bytes: u64) -> Entry {
        Entry {
            path: path.to_owned(),
            bytes,
            content_id: String::new(),
        }
    }

    fn manifest_entry() -> Entry {
        entry(MANIFEST_NAME, 412)
    }

    #[test]
    fn every_code_is_distinct_and_carries_the_registry_prefix() {
        let names: std::collections::BTreeSet<&str> =
            InstallCode::ALL.iter().map(|code| code.as_str()).collect();
        assert_eq!(names.len(), InstallCode::ALL.len());
        assert!(names.iter().all(|name| name.starts_with("PLUGIN_")));
    }

    #[test]
    fn a_range_is_a_conjunction_of_comparators() {
        let range = EngineRange::parse(">=0.1.0 <0.2.0").expect("parses");
        assert!(range.admits(&Version::parse("0.1.0").unwrap()));
        assert!(range.admits(&Version::parse("0.1.7").unwrap()));
        assert!(!range.admits(&Version::parse("0.2.0").unwrap()));
        assert!(!range.admits(&Version::parse("0.0.9").unwrap()));
    }

    #[test]
    fn versions_compare_numerically_rather_than_as_strings() {
        // Compared as strings, 0.10.0 sorts below 0.9.0 and a working build would be refused.
        let range = EngineRange::parse(">=0.9.0").expect("parses");
        assert!(range.admits(&Version::parse("0.10.0").unwrap()));
        assert!(Version::parse("0.10.0").unwrap() > Version::parse("0.9.0").unwrap());
    }

    #[test]
    fn a_shorthand_from_another_ecosystem_is_not_a_range() {
        for text in [
            "^0.1.0",
            "~1.2.3",
            ">=0.1.*",
            ">=0.1",
            ">=1.0.0-beta.1",
            "",
            "0.1.0",
            ">=0.01.0",
        ] {
            assert!(
                EngineRange::parse(text).is_err(),
                "`{text}` parsed, and the grammar admits only comparators"
            );
        }
    }

    #[test]
    fn a_tree_at_the_root_is_accepted_in_path_order() {
        let planned = plan(
            &[
                entry("bin/tool", 10),
                manifest_entry(),
                entry("README.md", 20),
            ],
            None,
        )
        .expect("accepted");
        let paths: Vec<&str> = planned.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["README.md", "bin/tool", MANIFEST_NAME]);
    }

    #[test]
    fn a_subdirectory_narrows_the_tree_rather_than_rejecting_what_is_outside() {
        let planned = plan(
            &[
                entry("README.md", 20),
                entry("plugins/viewer/nostdb-plugin.json", 412),
                entry("plugins/viewer/bin/tool", 10),
            ],
            Some("plugins/viewer"),
        )
        .expect("accepted");
        let paths: Vec<&str> = planned.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["bin/tool", MANIFEST_NAME]);
        // The read still asks for the path the snapshot has.
        assert_eq!(planned[0].source_path, "plugins/viewer/bin/tool");
    }

    #[test]
    fn a_tree_that_is_not_a_plugin_is_refused_as_a_source() {
        for (entries, subdirectory) in [
            (vec![entry("README.md", 20)], None),
            (vec![manifest_entry(), entry("../outside", 4)], None),
            (vec![manifest_entry(), entry("/etc/hosts", 4)], None),
            (vec![manifest_entry(), entry("bin/AUX.txt", 4)], None),
            (vec![manifest_entry(), entry("bin/tool.", 4)], None),
            (vec![manifest_entry(), entry("bin//tool", 4)], None),
            (vec![manifest_entry(), entry("C:/windows", 4)], None),
            (vec![manifest_entry(), entry("bin\\tool", 4)], None),
            (vec![entry("nested/nostdb-plugin.json", 412)], None),
            (vec![manifest_entry()], Some("absent")),
        ] {
            let error = plan(&entries, subdirectory).expect_err("refused");
            assert_eq!(
                error.code,
                InstallCode::SourceInvalid,
                "{entries:?} reported {}",
                error.code
            );
        }
    }

    #[test]
    fn every_limit_is_refused_one_past_it_and_accepted_exactly_on_it() {
        // Entry count.
        let mut exact = vec![manifest_entry()];
        for index in 1..MAX_ENTRIES {
            exact.push(entry(&format!("assets/f{index}"), 1));
        }
        assert_eq!(exact.len(), MAX_ENTRIES);
        assert!(plan(&exact, None).is_ok());
        exact.push(entry("assets/one-more", 1));
        assert_eq!(
            plan(&exact, None).expect_err("refused").code,
            InstallCode::LimitExceeded
        );

        // Entry bytes.
        assert!(plan(&[manifest_entry(), entry("big", MAX_ENTRY_BYTES)], None).is_ok());
        assert_eq!(
            plan(&[manifest_entry(), entry("big", MAX_ENTRY_BYTES + 1)], None)
                .expect_err("refused")
                .code,
            InstallCode::LimitExceeded
        );

        // Total bytes, with every entry individually legal.
        let mut whole = vec![entry(MANIFEST_NAME, 0)];
        for index in 0..8 {
            whole.push(entry(&format!("bin/part-{index}"), MAX_TOTAL_BYTES / 8));
        }
        assert!(plan(&whole, None).is_ok());
        whole.push(entry("bin/one-more", 1));
        assert_eq!(
            plan(&whole, None).expect_err("refused").code,
            InstallCode::LimitExceeded
        );

        // Path length.
        let exactly = format!("b/{}", "n".repeat(MAX_PATH_BYTES - 2));
        assert_eq!(exactly.len(), MAX_PATH_BYTES);
        assert!(plan(&[manifest_entry(), entry(&exactly, 1)], None).is_ok());
        assert_eq!(
            plan(&[manifest_entry(), entry(&format!("{exactly}n"), 1)], None)
                .expect_err("refused")
                .code,
            InstallCode::LimitExceeded
        );

        // Path depth.
        let deep: Vec<String> = (0..MAX_PATH_DEPTH)
            .map(|index| format!("d{index}"))
            .collect();
        let exactly = deep.join("/");
        assert_eq!(exactly.split('/').count(), MAX_PATH_DEPTH);
        assert!(plan(&[manifest_entry(), entry(&exactly, 1)], None).is_ok());
        assert_eq!(
            plan(
                &[manifest_entry(), entry(&format!("{exactly}/leaf"), 1)],
                None
            )
            .expect_err("refused")
            .code,
            InstallCode::LimitExceeded
        );
    }

    #[test]
    fn a_tree_digest_is_reproducible_and_order_independent() {
        let forwards = tree_digest(&[
            ("a".to_owned(), b"one".to_vec()),
            ("b".to_owned(), b"two".to_vec()),
        ]);
        let backwards = tree_digest(&[
            ("b".to_owned(), b"two".to_vec()),
            ("a".to_owned(), b"one".to_vec()),
        ]);
        assert_eq!(forwards, backwards, "a host's enumeration order changed it");
        assert!(forwards.starts_with("sha256:"));

        // A path change alone changes the digest, which is what covers a renamed file.
        let renamed = tree_digest(&[
            ("a".to_owned(), b"one".to_vec()),
            ("c".to_owned(), b"two".to_vec()),
        ]);
        assert_ne!(forwards, renamed);
    }

    #[test]
    fn a_record_round_trips_through_its_own_rendering() {
        let mut record = Record::default();
        record.upsert(Installation {
            name: "org.example.beta".to_owned(),
            repository: "https://github.com/example/beta".to_owned(),
            commit: "b".repeat(40),
            subdirectory: None,
            manifest_digest: format!("sha256:{}", "1".repeat(64)),
            tree_digest: format!("sha256:{}", "2".repeat(64)),
            scope: Scope::Project,
            manifest_version: 1,
            plugin_version: "2.0.0".to_owned(),
            approved_permissions: serde_json::json!({"database_write": false}),
        });
        record.upsert(Installation {
            name: "org.example.alpha".to_owned(),
            repository: "https://github.com/example/alpha".to_owned(),
            commit: "a".repeat(40),
            subdirectory: Some("sub".to_owned()),
            manifest_digest: format!("sha256:{}", "3".repeat(64)),
            tree_digest: format!("sha256:{}", "4".repeat(64)),
            scope: Scope::Project,
            manifest_version: 1,
            plugin_version: "1.0.0".to_owned(),
            approved_permissions: serde_json::json!({"database_write": false}),
        });

        let text = record.to_json();
        let read = Record::parse(&text, Scope::Project).expect("reads");
        assert_eq!(read, record);
        // Inserted out of order and rendered in order, so two managers agree on the file.
        let names: Vec<&str> = read.installed().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["org.example.alpha", "org.example.beta"]);
    }

    #[test]
    fn a_record_reports_every_problem_rather_than_the_first() {
        let text = serde_json::json!({
            "plugin_install_version": 1,
            "installed": [{
                "name": "org.example.viewer",
                "repository": "https://github.com/example/viewer",
                "commit": "main",
                "manifest_digest": "abc",
                "tree_digest": format!("sha256:{}", "2".repeat(64)),
                "scope": "project",
                "manifest_version": 1,
                "plugin_version": "1.0.0",
                "approved_permissions": {"database_write": true}
            }]
        })
        .to_string();
        let (code, problems) = Record::parse(&text, Scope::Project).expect_err("refused");
        assert_eq!(code, InstallCode::RecordInvalid);
        assert!(problems.len() >= 3, "reported only {problems:?}");
    }

    #[test]
    fn an_unsupported_record_version_is_reported_alone() {
        let text = r#"{"plugin_install_version": 2, "installed": []}"#;
        let (code, problems) = Record::parse(text, Scope::Project).expect_err("refused");
        assert_eq!(code, InstallCode::RecordVersionUnsupported);
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_record_claiming_the_other_scope_is_refused() {
        let text = serde_json::json!({
            "plugin_install_version": 1,
            "installed": [{
                "name": "org.example.viewer",
                "repository": "https://github.com/example/viewer",
                "commit": "a".repeat(40),
                "manifest_digest": format!("sha256:{}", "1".repeat(64)),
                "tree_digest": format!("sha256:{}", "2".repeat(64)),
                "scope": "global",
                "manifest_version": 1,
                "plugin_version": "1.0.0",
                "approved_permissions": {"database_write": false}
            }]
        })
        .to_string();
        assert_eq!(
            Record::parse(&text, Scope::Project).expect_err("refused").0,
            InstallCode::RecordInvalid
        );
    }

    #[test]
    fn a_source_with_no_ref_cannot_be_turned_into_a_locator() {
        let source = PluginSource::parse("https://github.com/example/viewer").expect("parses");
        let error = locator_for(&source).expect_err("refused");
        assert_eq!(error.code, InstallCode::SourceInvalid);
        assert!(error.reason.contains("ref"));

        let pinned =
            PluginSource::parse("https://github.com/Example/Viewer?ref=v1.2.3").expect("parses");
        assert_eq!(
            locator_for(&pinned).expect("a locator"),
            "github://example/viewer/?ref=v1.2.3"
        );
    }

    #[test]
    fn every_install_code_maps_to_a_documented_exit_class() {
        for code in InstallCode::ALL {
            let class = code.exit_class();
            assert!(
                matches!(class, ExitClass::Validation | ExitClass::Plugin),
                "{code} reports {class}"
            );
        }
        assert_eq!(
            InstallCode::DigestMismatch.exit_class(),
            ExitClass::Plugin,
            "a plugin that is not what was approved is not a document somebody edits"
        );
    }
}
