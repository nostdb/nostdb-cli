//! The plugin manager's vocabulary.
//!
//! The native plugin manager exists once, and it is here. A second registry in the Skill
//! would mean two answers to "what is installed", and the one a user got would depend on
//! which surface they reached for.
//!
//! # Reading is not installing, and installing is not running
//!
//! This module reads: it parses a source, validates a manifest, and says what an
//! installation would record. Nothing here fetches, writes, or executes.
//!
//! That separation is the contract's, not a convenience. Installation MUST NOT execute
//! plugin code, and the way to be sure of that is for the code that decides whether a plugin
//! is acceptable to have no way to run one.

/// Why a plugin manifest was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginCode {
    /// The manifest breaks a rule decidable by reading it.
    ///
    /// Every problem found is reported rather than the first, so an author fixes a manifest
    /// in one pass rather than one failed install per mistake.
    ManifestInvalid,
    /// The `manifest_version` is not one this build reads.
    ///
    /// Reported separately from an invalid manifest, because nothing after an unreadable
    /// version is interpretable and naming a malformed field would send an author looking
    /// for one that is not there.
    ManifestVersionUnsupported,
}

impl PluginCode {
    /// Every code, so a test can walk them.
    pub const ALL: [Self; 2] = [Self::ManifestInvalid, Self::ManifestVersionUnsupported];

    /// The symbolic name a refusal carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManifestInvalid => "PLUGIN_MANIFEST_INVALID",
            Self::ManifestVersionUnsupported => "PLUGIN_MANIFEST_VERSION_UNSUPPORTED",
        }
    }

    /// The exit class a refusal reports.
    ///
    /// Both are validation failures: the manifest is a document, and a document that will
    /// not do is the same kind of problem however it fails.
    #[must_use]
    pub const fn exit_class(self) -> crate::exit::ExitClass {
        crate::exit::ExitClass::Validation
    }
}

impl std::fmt::Display for PluginCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Where a plugin is installed from.
///
/// ```text
/// https://github.com/<owner>/<repository>[?ref=<git-ref>][#<subdirectory>]
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSource {
    owner: String,
    repository: String,
    reference: String,
    subdirectory: Option<String>,
}

impl PluginSource {
    /// The owner, lowered.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The repository, lowered.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// The ref the author named.
    ///
    /// Required, and resolved once to a commit. A user does not follow a branch: they pin what
    /// they installed, and moving the branch does not move them.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// The subdirectory within the repository, when one was named.
    #[must_use]
    pub fn subdirectory(&self) -> Option<&str> {
        self.subdirectory.as_deref()
    }

    /// Parses a plugin source.
    ///
    /// # Errors
    ///
    /// Returns a reason. Every refusal carries [`PluginCode::ManifestInvalid`], because a
    /// source that cannot be read is a request that cannot be acted on.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let rest = text
            .strip_prefix("https://github.com/")
            .ok_or_else(|| "the MVP plugin source is GitHub".to_owned())?;
        if text.contains('@') {
            // Refused rather than stripped: somebody who wrote one meant to use it, and
            // dropping it quietly turns an authentication mistake into "not found".
            return Err("a credential must never appear in a plugin source".to_owned());
        }

        let (rest, subdirectory) = match rest.split_once('#') {
            Some((rest, sub)) => (rest, Some(sub)),
            None => (rest, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((rest, query)) => (rest, Some(query)),
            None => (rest, None),
        };

        let mut segments = rest.trim_end_matches('/').split('/');
        let owner = segments.next().unwrap_or_default();
        let repository = segments.next().unwrap_or_default();
        if owner.is_empty() || repository.is_empty() {
            return Err("a source names an owner and a repository".to_owned());
        }
        if segments.next().is_some() {
            return Err("a subdirectory is written after `#`, not as a path segment".to_owned());
        }

        // A ref is required, not defaulted. A manager retrieves through a provider, and the
        // provider protocol requires every locator to carry one and forbids inventing one,
        // because a default branch can change and a locator is an identity. Requiring it here is
        // also what makes an install visible in the command that performed it.
        let reference = {
            let query = query.ok_or_else(|| {
                "a source names a ref: `?ref=<git-ref>`, resolved once to a commit".to_owned()
            })?;
            let value = query
                .strip_prefix("ref=")
                .ok_or_else(|| format!("`{query}` is not `ref=<git-ref>`"))?;
            if value.is_empty() {
                return Err("the ref is empty".to_owned());
            }
            value.to_owned()
        };

        if let Some(sub) = subdirectory {
            // A subdirectory names something inside the repository. One that escapes is
            // naming something the plugin author does not ship.
            if sub.starts_with('/') || sub.split('/').any(|part| part == "..") {
                return Err("a subdirectory names something inside the repository".to_owned());
            }
            if sub.is_empty() {
                return Err("the subdirectory is empty".to_owned());
            }
        }

        Ok(Self {
            // Lowered for the same reason a `github://` locator lowers them: GitHub treats
            // them case-insensitively, so one repository is one identity however it is typed.
            owner: owner.to_ascii_lowercase(),
            repository: repository.to_ascii_lowercase(),
            reference,
            subdirectory: subdirectory.map(str::to_owned),
        })
    }
}

impl std::fmt::Display for PluginSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "https://github.com/{}/{}?ref={}",
            self.owner, self.repository, self.reference
        )?;
        if let Some(subdirectory) = &self.subdirectory {
            write!(formatter, "#{subdirectory}")?;
        }
        Ok(())
    }
}

/// The manifest versions this build reads.
pub const SUPPORTED_MANIFEST_VERSIONS: [u64; 1] = [1];

/// Validates a manifest against the published contract.
///
/// Returns every problem found rather than the first, so an author fixes a manifest in one
/// pass rather than one failed install per mistake.
///
/// # Errors
///
/// Returns the code and the problems. An unsupported version is returned alone: nothing
/// after an unreadable version is interpretable, and naming a malformed field would send an
/// author looking for one that is not there.
pub fn validate_manifest(text: &str) -> Result<(), (PluginCode, Vec<String>)> {
    let invalid = |reasons: Vec<String>| (PluginCode::ManifestInvalid, reasons);
    let document: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| invalid(vec![format!("the manifest is not JSON: {error}")]))?;
    let Some(root) = document.as_object() else {
        return Err(invalid(vec!["the manifest is not an object".to_owned()]));
    };

    match root
        .get("manifest_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(found) if SUPPORTED_MANIFEST_VERSIONS.contains(&found) => {}
        Some(found) => {
            return Err((
                PluginCode::ManifestVersionUnsupported,
                vec![format!(
                    "manifest_version {found} is not one this build reads"
                )],
            ));
        }
        None => return Err(invalid(vec!["manifest_version is absent".to_owned()])),
    }

    let mut problems = Vec::new();
    for required in [
        "name",
        "version",
        "nostdb",
        "entrypoint",
        "protocol_version",
        "actions",
        "permissions",
    ] {
        if !root.contains_key(required) {
            // A manifest that did not say is not one asking for nothing.
            problems.push(format!("{required} is absent"));
        }
    }

    if let Some(name) = root.get("name").and_then(serde_json::Value::as_str) {
        let segments: Vec<&str> = name.split('.').collect();
        let named = segments.len() >= 2
            && segments.iter().all(|segment| {
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            });
        if !named {
            problems.push("name is not two or more lower-case dotted segments".to_owned());
        }
    }

    match root
        .get("entrypoint")
        .and_then(serde_json::Value::as_object)
        .and_then(|entry| entry.get("command"))
    {
        Some(serde_json::Value::Array(command)) if !command.is_empty() => {
            match command[0].as_str() {
                None => problems.push("the command's first element is not a string".to_owned()),
                Some(first) => {
                    // A plugin names something inside itself. One naming `/bin/sh` or an
                    // escaping path is naming something it did not ship.
                    if first.starts_with('/') || first.split('/').any(|part| part == "..") {
                        problems
                            .push("the command path is absolute or escapes the plugin".to_owned());
                    }
                }
            }
        }
        Some(serde_json::Value::Array(_)) => problems.push("the command is empty".to_owned()),
        // A string a shell interprets is the plugin author choosing what runs.
        Some(_) => problems.push("the command is not an argument vector".to_owned()),
        None => problems.push("entrypoint.command is absent".to_owned()),
    }

    if let Some(permissions) = root
        .get("permissions")
        .and_then(serde_json::Value::as_object)
    {
        if permissions
            .get("database_write")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            // Refused by name rather than ignored: an author who asked has a
            // misunderstanding worth correcting.
            problems
                .push("database_write must be false; only the Engine writes .nostdb".to_owned());
        }
        if let Some(paths) = permissions
            .get("output_paths")
            .and_then(serde_json::Value::as_array)
        {
            for path in paths.iter().filter_map(serde_json::Value::as_str) {
                if path.starts_with('/') || path.split('/').any(|part| part == "..") {
                    // Rejected rather than clamped: clamping would grant something adjacent
                    // to what was asked, and the author would not know which.
                    problems.push(format!("output path `{path}` is absolute or escapes"));
                }
            }
        }
        if let Some(hosts) = permissions
            .get("network_hosts")
            .and_then(serde_json::Value::as_array)
        {
            if hosts.iter().any(|host| host.as_str() == Some("*")) {
                problems.push("`*` is not a host; nobody can meaningfully approve it".to_owned());
            }
        }
    }

    if let Some(actions) = root.get("actions").and_then(serde_json::Value::as_array) {
        for action in actions {
            if action
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_none()
            {
                problems.push("an action states no name".to_owned());
            }
            match action.get("ai_usage").and_then(serde_json::Value::as_str) {
                Some("none" | "optional" | "required") => {}
                // An action nobody can budget for.
                _ => problems.push("an action states no known ai_usage".to_owned()),
            }
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(invalid(problems))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_distinct_and_carries_the_registry_prefix() {
        let names: std::collections::BTreeSet<&str> =
            PluginCode::ALL.iter().map(|code| code.as_str()).collect();
        assert_eq!(names.len(), PluginCode::ALL.len());
        assert!(
            names
                .iter()
                .all(|name| name.starts_with("PLUGIN_MANIFEST_"))
        );
    }

    #[test]
    fn a_refused_manifest_is_a_validation_failure() {
        for code in PluginCode::ALL {
            assert_eq!(code.exit_class(), crate::exit::ExitClass::Validation);
        }
    }
}
