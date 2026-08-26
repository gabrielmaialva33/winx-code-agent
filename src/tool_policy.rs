//! Per-principal MCP tool catalog policies.

use serde::Deserialize;

use crate::errors::{Result, WinxError};
use crate::tool_registry::ToolKind;

pub use crate::tool_registry::ALL_TOOL_NAMES;

/// Curated catalog shapes for common MCP clients and workflows.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ToolProfile {
    /// Advertise and permit every Winx tool.
    #[default]
    Full,
    /// Core repository exploration and editing without image or handoff helpers.
    Coding,
    /// Filesystem read/navigation tools only; shell and edit calls are rejected.
    ReadOnly,
    /// Durable terminal access with the smallest useful catalog.
    Terminal,
}

/// Exact authority for normalized file mutations. Public tool names and edit
/// authority are deliberately separate during the hidden-alias migration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EditPermissionSet {
    bits: u16,
}

impl EditPermissionSet {
    const SINGLE_REPLACE: u16 = 1 << 0;
    const SINGLE_SEARCH_REPLACE: u16 = 1 << 1;
    const SINGLE_LINE_PATCH: u16 = 1 << 2;
    const SINGLE_UNDO: u16 = 1 << 3;
    const BATCH_REPLACE: u16 = 1 << 4;
    const BATCH_SEARCH_REPLACE: u16 = 1 << 5;
    const BATCH_LINE_PATCH: u16 = 1 << 6;
    const VERIFY: u16 = 1 << 7;

    const ALL: Self = Self {
        bits: Self::SINGLE_REPLACE
            | Self::SINGLE_SEARCH_REPLACE
            | Self::SINGLE_LINE_PATCH
            | Self::SINGLE_UNDO
            | Self::BATCH_REPLACE
            | Self::BATCH_SEARCH_REPLACE
            | Self::BATCH_LINE_PATCH,
    };

    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    pub const fn has_mutation_authority(self) -> bool {
        self.bits & !Self::VERIFY != 0
    }

    pub const fn allows(self, mode: crate::tools::edit_files::EditMode, file_count: usize) -> bool {
        let batch = file_count > 1;
        let bit = match (mode, batch) {
            (crate::tools::edit_files::EditMode::Replace, false) => Self::SINGLE_REPLACE,
            (crate::tools::edit_files::EditMode::SearchReplace, false) => {
                Self::SINGLE_SEARCH_REPLACE
            }
            (crate::tools::edit_files::EditMode::LinePatch, false) => Self::SINGLE_LINE_PATCH,
            (crate::tools::edit_files::EditMode::Undo, false) => Self::SINGLE_UNDO,
            (crate::tools::edit_files::EditMode::Replace, true) => Self::BATCH_REPLACE,
            (crate::tools::edit_files::EditMode::SearchReplace, true) => Self::BATCH_SEARCH_REPLACE,
            (crate::tools::edit_files::EditMode::LinePatch, true) => Self::BATCH_LINE_PATCH,
            (crate::tools::edit_files::EditMode::Undo, true) => 0,
        };
        self.bits & bit != 0
    }

    pub const fn allows_verification(self) -> bool {
        self.bits & Self::VERIFY != 0
    }

    pub const fn for_legacy_tool(tool: ToolKind) -> Self {
        let bits = match tool {
            ToolKind::FileWriteOrEdit => Self::SINGLE_REPLACE | Self::SINGLE_SEARCH_REPLACE,
            ToolKind::MultiFileEdit => Self::BATCH_REPLACE | Self::BATCH_SEARCH_REPLACE,
            ToolKind::ApplyPatch => Self::SINGLE_LINE_PATCH,
            ToolKind::UndoEdit => Self::SINGLE_UNDO,
            _ => 0,
        };
        Self { bits }
    }

    pub(crate) const fn all_mutations() -> Self {
        Self::ALL
    }

    const fn union(self, other: Self) -> Self {
        Self { bits: self.bits | other.bits }
    }
}

impl ToolProfile {
    const fn mask(self) -> u16 {
        match self {
            Self::Full => {
                ToolKind::Initialize.bit()
                    | ToolKind::BashCommand.bit()
                    | ToolKind::ReadFiles.bit()
                    | ToolKind::FileWriteOrEdit.bit()
                    | ToolKind::MultiFileEdit.bit()
                    | ToolKind::VerifyEdit.bit()
                    | ToolKind::UndoEdit.bit()
                    | ToolKind::ContextSave.bit()
                    | ToolKind::ReadImage.bit()
                    | ToolKind::CodeMap.bit()
                    | ToolKind::ApplyPatch.bit()
            }
            Self::Coding => {
                ToolKind::Initialize.bit()
                    | ToolKind::BashCommand.bit()
                    | ToolKind::ReadFiles.bit()
                    | ToolKind::FileWriteOrEdit.bit()
                    | ToolKind::MultiFileEdit.bit()
                    | ToolKind::VerifyEdit.bit()
                    | ToolKind::UndoEdit.bit()
                    | ToolKind::CodeMap.bit()
                    | ToolKind::ApplyPatch.bit()
            }
            Self::ReadOnly => {
                ToolKind::Initialize.bit()
                    | ToolKind::ReadFiles.bit()
                    | ToolKind::ReadImage.bit()
                    | ToolKind::CodeMap.bit()
            }
            Self::Terminal => ToolKind::Initialize.bit() | ToolKind::BashCommand.bit(),
        }
    }
}

/// Fast, immutable allowlist used both for catalog filtering and call routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolPolicy {
    mask: u16,
    /// Phase-1 capability flag. It is deliberately separate from the stable
    /// public `ToolKind` enum and never contributes to `tools/list`.
    dark_edit_files: bool,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self::from_profile(ToolProfile::Full)
    }
}

impl ToolPolicy {
    /// Build a policy from one curated profile.
    pub const fn from_profile(profile: ToolProfile) -> Self {
        Self {
            mask: profile.mask(),
            dark_edit_files: matches!(profile, ToolProfile::Full | ToolProfile::Coding),
        }
    }

    /// Build an exact allowlist. Names are case-sensitive and duplicates are harmless.
    pub fn from_allowed_tools<I, S>(names: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut mask = 0_u16;
        let mut dark_edit_files = false;
        let mut saw_name = false;
        for name in names {
            saw_name = true;
            let name = name.as_ref();
            if name == "EditFiles" {
                dark_edit_files = true;
                continue;
            }
            let kind = ToolKind::parse(name).ok_or_else(|| {
                WinxError::ConfigurationError(format!(
                    "unknown MCP tool {name:?}; expected one of {}, EditFiles",
                    ALL_TOOL_NAMES.join(", ")
                ))
            })?;
            mask |= kind.bit();
        }
        if !saw_name {
            return Err(WinxError::ConfigurationError(
                "MCP tool allowlist cannot be empty".to_string(),
            ));
        }
        if mask & ToolKind::VerifyEdit.bit() != 0 && mask & ToolKind::BashCommand.bit() == 0 {
            return Err(WinxError::ConfigurationError(
                "VerifyEdit requires BashCommand in the same MCP tool allowlist".to_string(),
            ));
        }
        Ok(Self { mask, dark_edit_files })
    }

    /// Use an explicit allowlist when present, otherwise use the selected profile.
    pub fn resolve(profile: ToolProfile, allowed_tools: Option<&[String]>) -> Result<Self> {
        allowed_tools.map_or_else(
            || Ok(Self::from_profile(profile)),
            |names| Self::from_allowed_tools(names.iter()),
        )
    }

    /// Whether a tool may be advertised and called.
    pub fn allows(self, name: &str) -> bool {
        if name == "EditFiles" {
            self.allows_dark_edit_files()
        } else {
            ToolKind::parse(name).is_some_and(|kind| self.allows_kind(kind))
        }
    }

    /// Typed policy check used after the wire name has been parsed once.
    pub const fn allows_kind(self, kind: ToolKind) -> bool {
        self.mask & kind.bit() != 0
    }

    /// Whether the hidden unified edit alias may execute. Existing legacy edit
    /// grants dark access only to the exact modes/cardinality they already own.
    pub const fn allows_dark_edit_files(self) -> bool {
        self.dark_edit_files || self.edit_permissions().has_mutation_authority()
    }

    pub const fn edit_permissions(self) -> EditPermissionSet {
        let mut permissions = EditPermissionSet { bits: 0 };
        if self.dark_edit_files {
            permissions = permissions.union(EditPermissionSet::ALL);
        }
        if self.allows_kind(ToolKind::FileWriteOrEdit) {
            permissions =
                permissions.union(EditPermissionSet::for_legacy_tool(ToolKind::FileWriteOrEdit));
        }
        if self.allows_kind(ToolKind::MultiFileEdit) {
            permissions =
                permissions.union(EditPermissionSet::for_legacy_tool(ToolKind::MultiFileEdit));
        }
        if self.allows_kind(ToolKind::ApplyPatch) {
            permissions =
                permissions.union(EditPermissionSet::for_legacy_tool(ToolKind::ApplyPatch));
        }
        if self.allows_kind(ToolKind::UndoEdit) {
            permissions = permissions.union(EditPermissionSet::for_legacy_tool(ToolKind::UndoEdit));
        }
        if self.allows_kind(ToolKind::BashCommand) {
            permissions = permissions.union(EditPermissionSet { bits: EditPermissionSet::VERIFY });
        }
        permissions
    }

    /// Allowed names in stable catalog order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        ToolKind::ALL.into_iter().filter(move |kind| self.allows_kind(*kind)).map(ToolKind::as_str)
    }

    /// Number of advertised tools.
    pub fn len(self) -> usize {
        self.names().count()
    }

    /// Whether this policy advertises no tools.
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{ToolPolicy, ToolProfile, ALL_TOOL_NAMES};
    use crate::tools::edit_files::EditMode;

    #[test]
    fn profiles_have_stable_expected_catalogs() {
        assert_eq!(ToolPolicy::default().names().collect::<Vec<_>>(), ALL_TOOL_NAMES);
        assert_eq!(
            ToolPolicy::from_profile(ToolProfile::Coding).names().collect::<Vec<_>>(),
            vec![
                "Initialize",
                "BashCommand",
                "ReadFiles",
                "FileWriteOrEdit",
                "MultiFileEdit",
                "VerifyEdit",
                "UndoEdit",
                "CodeMap",
                "ApplyPatch",
            ]
        );
        assert_eq!(
            ToolPolicy::from_profile(ToolProfile::ReadOnly).names().collect::<Vec<_>>(),
            vec!["Initialize", "ReadFiles", "ReadImage", "CodeMap"]
        );
        assert_eq!(
            ToolPolicy::from_profile(ToolProfile::Terminal).names().collect::<Vec<_>>(),
            vec!["Initialize", "BashCommand"]
        );
    }

    #[test]
    fn explicit_allowlist_is_exact_deduplicated_and_validated() {
        let policy = ToolPolicy::from_allowed_tools(["ReadFiles", "Initialize", "ReadFiles"])
            .expect("valid policy");
        assert_eq!(policy.names().collect::<Vec<_>>(), vec!["Initialize", "ReadFiles"]);
        assert!(ToolPolicy::from_allowed_tools(["readfiles"]).is_err());
        assert!(ToolPolicy::from_allowed_tools(Vec::<String>::new()).is_err());
        assert!(ToolPolicy::from_allowed_tools(["VerifyEdit"]).is_err());
        assert!(ToolPolicy::from_allowed_tools(["VerifyEdit", "BashCommand"]).is_ok());
    }

    #[test]
    fn edit_permissions_preserve_legacy_cardinality_and_require_bash_for_verification() {
        let single = ToolPolicy::from_allowed_tools(["FileWriteOrEdit"]).expect("valid policy");
        let permissions = single.edit_permissions();
        assert!(permissions.allows(EditMode::Replace, 1));
        assert!(permissions.allows(EditMode::SearchReplace, 1));
        assert!(!permissions.allows(EditMode::Replace, 2));
        assert!(!permissions.allows(EditMode::LinePatch, 1));
        assert!(!permissions.allows_verification());

        let verified = ToolPolicy::from_allowed_tools(["FileWriteOrEdit", "BashCommand"])
            .expect("valid policy")
            .edit_permissions();
        assert!(verified.allows_verification());

        let dark = ToolPolicy::from_allowed_tools(["EditFiles"]).expect("valid dark policy");
        assert!(dark.names().next().is_none());
        assert!(dark.is_empty());
        assert!(dark.edit_permissions().allows(EditMode::LinePatch, 2));
        assert!(!dark.edit_permissions().allows_verification());

        for (name, allowed_mode, count) in [
            ("FileWriteOrEdit", EditMode::Replace, 1),
            ("ApplyPatch", EditMode::LinePatch, 1),
            ("MultiFileEdit", EditMode::SearchReplace, 2),
        ] {
            let policy = ToolPolicy::from_allowed_tools([name]).expect("valid legacy policy");
            assert!(policy.allows_dark_edit_files());
            assert!(policy.edit_permissions().allows(allowed_mode, count));
            assert!(!policy.edit_permissions().allows_verification());
        }

        let bash_only = ToolPolicy::from_allowed_tools(["BashCommand"]).expect("valid policy");
        assert!(!bash_only.allows_dark_edit_files());
    }
}
