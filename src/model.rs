use std::{path::PathBuf, time::Instant};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChangeKind {
    Create,
    Modify,
    Rename,
    Delete,
    Git,
    Work,
    Integrity,
}

impl ChangeKind {
    pub const ALL: [Self; 7] = [
        Self::Modify,
        Self::Create,
        Self::Rename,
        Self::Delete,
        Self::Git,
        Self::Work,
        Self::Integrity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Modify => "MODIFY",
            Self::Rename => "MOVE",
            Self::Delete => "DELETE",
            Self::Git => "GIT",
            Self::Work => "WORK",
            Self::Integrity => "INTEGRITY",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Create => "+",
            Self::Modify => "~",
            Self::Rename => ">",
            Self::Delete => "-",
            Self::Git => "◆",
            Self::Work => "●",
            Self::Integrity => "!",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetKind {
    File,
    Directory,
    Repository,
    WorkItem,
    Observer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrityLevel {
    Info,
    Degraded,
    Uncertain,
    Lost,
}

impl IntegrityLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Degraded => "DEGRADED",
            Self::Uncertain => "UNCERTAIN",
            Self::Lost => "LOST",
        }
    }

    pub fn severity(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Degraded => 1,
            Self::Uncertain => 2,
            Self::Lost => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct IntegrityEvent {
    pub level: IntegrityLevel,
    pub source: &'static str,
    pub summary: String,
    pub detail: String,
    pub root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffKind {
    Add,
    Remove,
    Equal,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old_number: Option<usize>,
    pub new_number: Option<usize>,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct ChangeEvent {
    pub id: u64,
    pub kind: ChangeKind,
    pub target: TargetKind,
    pub path: PathBuf,
    pub previous_path: Option<PathBuf>,
    pub root: PathBuf,
    pub occurred_at: Instant,
    pub size: u64,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub diff: Vec<DiffLine>,
    pub detail: Option<String>,
    pub integrity_level: Option<IntegrityLevel>,
}
