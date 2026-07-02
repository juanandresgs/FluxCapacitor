use std::{path::PathBuf, time::Instant};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChangeKind {
    Create,
    Modify,
    Rename,
    Delete,
}

impl ChangeKind {
    pub const ALL: [Self; 4] = [Self::Modify, Self::Create, Self::Rename, Self::Delete];

    pub fn label(self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Modify => "MODIFY",
            Self::Rename => "MOVE",
            Self::Delete => "DELETE",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Create => "+",
            Self::Modify => "~",
            Self::Rename => ">",
            Self::Delete => "-",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetKind {
    File,
    Directory,
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
}
