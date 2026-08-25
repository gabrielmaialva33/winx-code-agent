//! Typed source of truth for MCP tool identity and cross-cutting behavior.

use std::fmt;

/// Stable tool order used by discovery, policy bits, and dispatch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum ToolKind {
    Initialize = 0,
    BashCommand = 1,
    ReadFiles = 2,
    FileWriteOrEdit = 3,
    MultiFileEdit = 4,
    VerifyEdit = 5,
    UndoEdit = 6,
    ContextSave = 7,
    ReadImage = 8,
    CodeMap = 9,
    ApplyPatch = 10,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolAccess {
    ReadOnly,
    Neutral,
    Destructive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolOutputContract {
    Shared,
    CodeMap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolWorld {
    Closed,
    Open,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolSessionContract {
    Initializes,
    RequiresInitialized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolDescriptor {
    pub access: ToolAccess,
    pub world: ToolWorld,
    pub session: ToolSessionContract,
    pub output_contract: ToolOutputContract,
}

impl ToolKind {
    pub const ALL: [Self; 11] = [
        Self::Initialize,
        Self::BashCommand,
        Self::ReadFiles,
        Self::FileWriteOrEdit,
        Self::MultiFileEdit,
        Self::VerifyEdit,
        Self::UndoEdit,
        Self::ContextSave,
        Self::ReadImage,
        Self::CodeMap,
        Self::ApplyPatch,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "Initialize",
            Self::BashCommand => "BashCommand",
            Self::ReadFiles => "ReadFiles",
            Self::FileWriteOrEdit => "FileWriteOrEdit",
            Self::MultiFileEdit => "MultiFileEdit",
            Self::VerifyEdit => "VerifyEdit",
            Self::UndoEdit => "UndoEdit",
            Self::ContextSave => "ContextSave",
            Self::ReadImage => "ReadImage",
            Self::CodeMap => "CodeMap",
            Self::ApplyPatch => "ApplyPatch",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "Initialize" => Some(Self::Initialize),
            "BashCommand" => Some(Self::BashCommand),
            "ReadFiles" => Some(Self::ReadFiles),
            "FileWriteOrEdit" => Some(Self::FileWriteOrEdit),
            "MultiFileEdit" => Some(Self::MultiFileEdit),
            "VerifyEdit" => Some(Self::VerifyEdit),
            "UndoEdit" => Some(Self::UndoEdit),
            "ContextSave" => Some(Self::ContextSave),
            "ReadImage" => Some(Self::ReadImage),
            "CodeMap" => Some(Self::CodeMap),
            "ApplyPatch" => Some(Self::ApplyPatch),
            _ => None,
        }
    }

    pub const fn bit(self) -> u16 {
        1_u16 << self as u8
    }

    pub const fn descriptor(self) -> ToolDescriptor {
        let (access, world) = match self {
            Self::Initialize | Self::ReadFiles | Self::ReadImage | Self::CodeMap => {
                (ToolAccess::ReadOnly, ToolWorld::Closed)
            }
            Self::ContextSave => (ToolAccess::Neutral, ToolWorld::Closed),
            Self::UndoEdit => (ToolAccess::Destructive, ToolWorld::Closed),
            Self::BashCommand
            | Self::FileWriteOrEdit
            | Self::MultiFileEdit
            | Self::ApplyPatch
            | Self::VerifyEdit => (ToolAccess::Destructive, ToolWorld::Open),
        };
        ToolDescriptor {
            access,
            world,
            session: if matches!(self, Self::Initialize) {
                ToolSessionContract::Initializes
            } else {
                ToolSessionContract::RequiresInitialized
            },
            output_contract: if matches!(self, Self::CodeMap) {
                ToolOutputContract::CodeMap
            } else {
                ToolOutputContract::Shared
            },
        }
    }

    pub const fn is_file_mutation(self) -> bool {
        matches!(self, Self::FileWriteOrEdit | Self::MultiFileEdit | Self::ApplyPatch)
    }

    pub const fn requires_bash_companion(self) -> bool {
        matches!(self, Self::VerifyEdit)
    }
}

impl fmt::Display for ToolKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Backwards-compatible name array derived from the typed stable order.
pub const ALL_TOOL_NAMES: [&str; 11] = [
    ToolKind::Initialize.as_str(),
    ToolKind::BashCommand.as_str(),
    ToolKind::ReadFiles.as_str(),
    ToolKind::FileWriteOrEdit.as_str(),
    ToolKind::MultiFileEdit.as_str(),
    ToolKind::VerifyEdit.as_str(),
    ToolKind::UndoEdit.as_str(),
    ToolKind::ContextSave.as_str(),
    ToolKind::ReadImage.as_str(),
    ToolKind::CodeMap.as_str(),
    ToolKind::ApplyPatch.as_str(),
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn names_bits_and_parser_are_one_to_one() {
        assert_eq!(ALL_TOOL_NAMES, ToolKind::ALL.map(ToolKind::as_str));
        let names = ToolKind::ALL.into_iter().map(ToolKind::as_str).collect::<HashSet<_>>();
        let bits = ToolKind::ALL.into_iter().map(ToolKind::bit).collect::<HashSet<_>>();
        assert_eq!(names.len(), ToolKind::ALL.len());
        assert_eq!(bits.len(), ToolKind::ALL.len());
        for kind in ToolKind::ALL {
            assert_eq!(ToolKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ToolKind::parse("readfiles"), None);
    }

    #[test]
    fn every_current_tool_is_principal_and_workspace_session_aware() {
        assert_eq!(ToolKind::Initialize.descriptor().session, ToolSessionContract::Initializes);
        assert!(ToolKind::ALL
            .into_iter()
            .filter(|kind| *kind != ToolKind::Initialize)
            .all(|kind| { kind.descriptor().session == ToolSessionContract::RequiresInitialized }));
    }
}
