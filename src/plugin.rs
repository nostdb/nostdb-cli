//! The plugin manager's vocabulary.
//!
//! The native plugin manager exists once, and it is here. A second registry in the Skill
//! would mean two answers to "what is installed", and the one a user got would depend on
//! which surface they reached for.
//!
//! This module currently declares only the codes a refusal carries. They are part of the
//! `manifest_version` contract this repository is the owner of, so owning them is a fact
//! about the contract rather than about how much of the manager is built — and the workspace
//! verifier checks that the registry's owner and the source that declares them agree.

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
