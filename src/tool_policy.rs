//! Per-principal MCP tool catalog policies.

use serde::Deserialize;

use crate::errors::{Result, WinxError};

/// Canonical tool names in the order advertised by `tools/list`.
pub const ALL_TOOL_NAMES: [&str; 9] = [
    "Initialize",
    "BashCommand",
    "ReadFiles",
    "FileWriteOrEdit",
    "MultiFileEdit",
    "UndoEdit",
    "ContextSave",
    "ReadImage",
    "CodeMap",
];

const INITIALIZE: u16 = 1 << 0;
const BASH_COMMAND: u16 = 1 << 1;
const READ_FILES: u16 = 1 << 2;
const FILE_WRITE_OR_EDIT: u16 = 1 << 3;
const MULTI_FILE_EDIT: u16 = 1 << 4;
const UNDO_EDIT: u16 = 1 << 5;
const CONTEXT_SAVE: u16 = 1 << 6;
const READ_IMAGE: u16 = 1 << 7;
const CODE_MAP: u16 = 1 << 8;

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

impl ToolProfile {
    const fn mask(self) -> u16 {
        match self {
            Self::Full => {
                INITIALIZE
                    | BASH_COMMAND
                    | READ_FILES
                    | FILE_WRITE_OR_EDIT
                    | MULTI_FILE_EDIT
                    | UNDO_EDIT
                    | CONTEXT_SAVE
                    | READ_IMAGE
                    | CODE_MAP
            }
            Self::Coding => {
                INITIALIZE
                    | BASH_COMMAND
                    | READ_FILES
                    | FILE_WRITE_OR_EDIT
                    | MULTI_FILE_EDIT
                    | UNDO_EDIT
                    | CODE_MAP
            }
            Self::ReadOnly => INITIALIZE | READ_FILES | READ_IMAGE | CODE_MAP,
            Self::Terminal => INITIALIZE | BASH_COMMAND,
        }
    }
}

/// Fast, immutable allowlist used both for catalog filtering and call routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolPolicy {
    mask: u16,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self::from_profile(ToolProfile::Full)
    }
}

impl ToolPolicy {
    /// Build a policy from one curated profile.
    pub const fn from_profile(profile: ToolProfile) -> Self {
        Self { mask: profile.mask() }
    }

    /// Build an exact allowlist. Names are case-sensitive and duplicates are harmless.
    pub fn from_allowed_tools<I, S>(names: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut mask = 0_u16;
        let mut saw_name = false;
        for name in names {
            saw_name = true;
            let name = name.as_ref();
            let bit = tool_bit(name).ok_or_else(|| {
                WinxError::ConfigurationError(format!(
                    "unknown MCP tool {name:?}; expected one of {}",
                    ALL_TOOL_NAMES.join(", ")
                ))
            })?;
            mask |= bit;
        }
        if !saw_name {
            return Err(WinxError::ConfigurationError(
                "MCP tool allowlist cannot be empty".to_string(),
            ));
        }
        Ok(Self { mask })
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
        tool_bit(name).is_some_and(|bit| self.mask & bit != 0)
    }

    /// Allowed names in stable catalog order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        ALL_TOOL_NAMES.into_iter().filter(move |name| self.allows(name))
    }

    /// Number of advertised tools.
    pub fn len(self) -> usize {
        self.mask.count_ones() as usize
    }

    /// Whether this policy advertises no tools.
    pub fn is_empty(self) -> bool {
        self.mask == 0
    }
}

fn tool_bit(name: &str) -> Option<u16> {
    ALL_TOOL_NAMES.iter().position(|candidate| *candidate == name).map(|index| 1 << index)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{ToolPolicy, ToolProfile, ALL_TOOL_NAMES};

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
                "UndoEdit",
                "CodeMap",
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
    }
}
