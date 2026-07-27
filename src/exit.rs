//! Process exit classes.
//!
//! Root PRD section 20.4 fixes these numbers, and they are normative: a script may branch
//! on one. The symbolic diagnostic code is the primary signal and the number follows it,
//! which is why every class below names the kind of failure rather than the command that
//! produced it.
//!
//! Class 1 is deliberately absent. A shell reports 1 for a great many things a process
//! did not choose, including an uncaught panic, so leaving it unassigned keeps "the
//! command reported a failure it understands" distinguishable from "something went wrong
//! before the command could say so".

use std::fmt;

/// The class a finished command reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum ExitClass {
    /// Success, including success with non-strict warnings.
    Success = 0,
    /// A malformed invocation, or query syntax outside the supported subset.
    Usage = 2,
    /// The input is well formed and invalid: a validation or format error.
    Validation = 3,
    /// A synchronization or transaction conflict.
    Conflict = 4,
    /// A required source was unavailable, or a strict-link check failed.
    Unavailable = 5,
    /// A credential or permission failure.
    Credential = 6,
    /// A plugin is required, or a plugin failed.
    Plugin = 7,
    /// An AI budget or analysis authorization failure.
    AiBudget = 8,
    /// An input or output failure, or a corrupt file.
    Io = 9,
    /// An invariant this build guarantees did not hold.
    Internal = 10,
}

impl ExitClass {
    /// Every class, so a test can walk them.
    pub const ALL: [Self; 10] = [
        Self::Success,
        Self::Usage,
        Self::Validation,
        Self::Conflict,
        Self::Unavailable,
        Self::Credential,
        Self::Plugin,
        Self::AiBudget,
        Self::Io,
        Self::Internal,
    ];

    /// The number the process reports.
    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// Reports whether this class means the command succeeded.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    /// A short name, for a message rather than for a machine.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Usage => "usage",
            Self::Validation => "validation",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::Credential => "credential",
            Self::Plugin => "plugin",
            Self::AiBudget => "ai-budget",
            Self::Io => "io",
            Self::Internal => "internal",
        }
    }

    /// The class a project failure reports.
    ///
    /// A missing or already-configured project is a usage mistake. A refused document —
    /// settings, baseline, `.nost`, or a container whose payloads do not decode — is a
    /// validation failure, and so is a link declaration the graph will not accept. A
    /// `.nost` holding unadopted changes is a conflict. Everything filesystem-shaped is
    /// I/O.
    ///
    /// One mapping rather than one per command. Three identical copies existed until
    /// adding two error variants made the compiler demand the same edit in each; they had
    /// not yet drifted, and consolidating them is what keeps that true.
    #[must_use]
    pub const fn for_project_error(error: &nostdb_core::project::ProjectError) -> Self {
        use nostdb_core::project::ProjectError;
        match error {
            ProjectError::NotFound { .. } | ProjectError::AlreadyConfigured { .. } => Self::Usage,
            ProjectError::Settings { .. }
            | ProjectError::Baseline { .. }
            | ProjectError::Nost { .. }
            | ProjectError::Link { .. }
            // A change set the graph would not accept is a validation failure: the input
            // was well formed and the result would have broken an invariant.
            | ProjectError::Build { .. }
            | ProjectError::Decode(_) => Self::Validation,
            // The two representations disagree and the command refused to pick one, which
            // is the same class `sync` reports for a conflict it will not resolve.
            ProjectError::NostUnsynchronized { .. } => Self::Conflict,
            ProjectError::Io { .. } | ProjectError::Storage(_) => Self::Io,
        }
    }
}

impl fmt::Display for ExitClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.as_str(), self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn the_numbers_are_the_ones_the_product_contract_fixes() {
        // These are normative. A script may branch on one, so a change here is a breaking
        // change rather than a refactor.
        assert_eq!(ExitClass::Success.code(), 0);
        assert_eq!(ExitClass::Usage.code(), 2);
        assert_eq!(ExitClass::Validation.code(), 3);
        assert_eq!(ExitClass::Conflict.code(), 4);
        assert_eq!(ExitClass::Unavailable.code(), 5);
        assert_eq!(ExitClass::Credential.code(), 6);
        assert_eq!(ExitClass::Plugin.code(), 7);
        assert_eq!(ExitClass::AiBudget.code(), 8);
        assert_eq!(ExitClass::Io.code(), 9);
        assert_eq!(ExitClass::Internal.code(), 10);
    }

    #[test]
    fn no_class_reports_one() {
        // A shell reports 1 for a great many things, including a panic. Leaving it
        // unassigned keeps a reported failure distinguishable from an unreported one.
        assert!(ExitClass::ALL.iter().all(|class| class.code() != 1));
    }

    #[test]
    fn every_class_is_distinct_and_only_success_succeeds() {
        let codes: BTreeSet<i32> = ExitClass::ALL.iter().map(|class| class.code()).collect();
        assert_eq!(codes.len(), ExitClass::ALL.len());
        assert_eq!(
            ExitClass::ALL
                .iter()
                .filter(|class| class.is_success())
                .count(),
            1
        );
    }
}
