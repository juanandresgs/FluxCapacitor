use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::mpsc::{self, Receiver},
};

use anyhow::{Context, Result};
use notify::{ErrorKind, Event, RecommendedWatcher, RecursiveMode, Watcher};
use similar::{ChangeTag, TextDiff};
use walkdir::{DirEntry, WalkDir};

use crate::model::{DiffKind, DiffLine, IntegrityEvent, IntegrityLevel};

const MAX_TEXT_BYTES: u64 = 1024 * 1024;
const MAX_DIFF_LINES: usize = 300;

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub content: Option<String>,
    pub size: u64,
    pub is_dir: bool,
    pub read_error: Option<String>,
}

pub struct WatchState {
    pub roots: Vec<PathBuf>,
    root_labels: HashMap<PathBuf, String>,
    pub snapshots: HashMap<PathBuf, Snapshot>,
    pub receiver: Receiver<notify::Result<Event>>,
    _watcher: RecommendedWatcher,
}

pub struct DiffResult {
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
    pub detail: Option<String>,
}

impl WatchState {
    pub fn start(roots: Vec<PathBuf>) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |result| {
            let _ = sender.send(result);
        })
        .context("failed to create filesystem watcher")?;

        let mut snapshots = HashMap::new();
        for root in &roots {
            watcher
                .watch(root, RecursiveMode::Recursive)
                .with_context(|| format!("failed to watch {}", root.display()))?;
            seed_snapshots(root, &mut snapshots);
        }

        let root_labels = build_root_labels(&roots);
        Ok(Self {
            roots,
            root_labels,
            snapshots,
            receiver,
            _watcher: watcher,
        })
    }

    pub fn root_for(&self, path: &Path) -> PathBuf {
        self.roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .cloned()
            .unwrap_or_else(|| self.roots[0].clone())
    }

    pub fn relative_path(&self, path: &Path) -> PathBuf {
        let root = self.root_for(path);
        path.strip_prefix(root)
            .ok()
            .filter(|value| !value.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.file_name().map(PathBuf::from).unwrap_or_default())
    }

    pub fn take_snapshot(&mut self, path: &Path) -> Snapshot {
        let snapshot = read_snapshot(path);
        self.snapshots.insert(path.to_path_buf(), snapshot.clone());
        snapshot
    }

    pub fn remove_snapshot(&mut self, path: &Path) -> Snapshot {
        self.snapshots.remove(path).unwrap_or_else(|| Snapshot {
            content: None,
            size: 0,
            is_dir: path.is_dir(),
            read_error: None,
        })
    }

    pub fn previous_snapshot(&self, path: &Path) -> Option<&Snapshot> {
        self.snapshots.get(path)
    }

    pub fn is_root(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| root == path)
    }

    pub fn root_label(&self, root: &Path) -> String {
        self.root_labels
            .get(root)
            .cloned()
            .unwrap_or_else(|| root.display().to_string())
    }

    pub fn root_index(&self, root: &Path) -> usize {
        self.roots
            .iter()
            .position(|candidate| candidate == root)
            .unwrap_or(0)
    }

    pub fn add_root(&mut self, root: PathBuf) -> Result<Option<IntegrityEvent>> {
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot access related folder {}", root.display()))?;
        if self.roots.contains(&root) {
            return Ok(None);
        }
        self._watcher
            .watch(&root, RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch related folder {}", root.display()))?;
        seed_snapshots(&root, &mut self.snapshots);
        self.roots.push(root.clone());
        self.root_labels = build_root_labels(&self.roots);
        Ok(Some(IntegrityEvent {
            level: IntegrityLevel::Info,
            source: "filesystem",
            summary: "related folder joined".into(),
            detail: "native recursive filesystem events are active".into(),
            root,
        }))
    }

    pub fn established_events(&self) -> Vec<IntegrityEvent> {
        self.roots
            .iter()
            .map(|root| IntegrityEvent {
                level: IntegrityLevel::Info,
                source: "filesystem",
                summary: "watch established".into(),
                detail: "native recursive filesystem events are active".into(),
                root: root.clone(),
            })
            .collect()
    }
}

fn build_root_labels(roots: &[PathBuf]) -> HashMap<PathBuf, String> {
    roots
        .iter()
        .map(|root| {
            let components = root
                .components()
                .map(|component| component.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>();
            let label = (1..=components.len())
                .find_map(|depth| {
                    let suffix = &components[components.len() - depth..];
                    let unique = roots.iter().filter(|candidate| {
                        let candidate_components = candidate
                            .components()
                            .map(|component| component.as_os_str().to_string_lossy().to_string())
                            .collect::<Vec<_>>();
                        candidate_components.len() >= depth
                            && candidate_components[candidate_components.len() - depth..] == *suffix
                    });
                    (unique.count() == 1).then(|| suffix.join("/"))
                })
                .unwrap_or_else(|| root.display().to_string());
            (root.clone(), label)
        })
        .collect()
}

pub fn integrity_from_notify_error(
    source: &'static str,
    error: notify::Error,
    fallback_root: &Path,
) -> IntegrityEvent {
    let root = error
        .paths
        .first()
        .cloned()
        .unwrap_or_else(|| fallback_root.to_path_buf());
    let description = error.to_string();
    let description_lower = description.to_lowercase();
    let (level, summary) = match error.kind {
        ErrorKind::MaxFilesWatch => (IntegrityLevel::Lost, "native watch limit reached"),
        ErrorKind::PathNotFound | ErrorKind::WatchNotFound => {
            (IntegrityLevel::Lost, "watch target unavailable")
        }
        ErrorKind::Generic(_)
            if description_lower.contains("overflow")
                || description_lower.contains("dropped")
                || description_lower.contains("rescan") =>
        {
            (
                IntegrityLevel::Uncertain,
                "native events may have been lost",
            )
        }
        ErrorKind::Io(_) | ErrorKind::Generic(_) | ErrorKind::InvalidConfig(_) => {
            (IntegrityLevel::Degraded, "native watcher reported an error")
        }
    };
    IntegrityEvent {
        level,
        source,
        summary: summary.into(),
        detail: description,
        root,
    }
}

pub fn ignored(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(value) = component else {
            return false;
        };
        matches!(
            value.to_str(),
            Some(".git" | "node_modules" | "target" | "dist" | "coverage")
        )
    })
}

pub fn read_snapshot(path: &Path) -> Snapshot {
    let Ok(metadata) = fs::metadata(path) else {
        return Snapshot {
            content: None,
            size: 0,
            is_dir: false,
            read_error: Some(format!("could not read metadata for {}", path.display())),
        };
    };

    if metadata.is_dir() {
        return Snapshot {
            content: None,
            size: 0,
            is_dir: true,
            read_error: None,
        };
    }

    if metadata.len() > MAX_TEXT_BYTES {
        return Snapshot {
            content: None,
            size: metadata.len(),
            is_dir: false,
            read_error: None,
        };
    }

    let (content, read_error) = match fs::read(path) {
        Ok(bytes) if bytes.iter().take(8192).any(|byte| *byte == 0) => (None, None),
        Ok(bytes) => (String::from_utf8(bytes).ok(), None),
        Err(error) => (
            None,
            Some(format!("could not read {}: {error}", path.display())),
        ),
    };

    Snapshot {
        content,
        size: metadata.len(),
        is_dir: false,
        read_error,
    }
}

pub fn build_diff(previous: Option<&str>, current: Option<&str>) -> DiffResult {
    let (Some(previous), Some(current)) = (previous, current) else {
        return DiffResult {
            lines: Vec::new(),
            added: 0,
            removed: 0,
            detail: Some("binary, unreadable, or larger than 1 MiB".into()),
        };
    };

    let diff = TextDiff::from_lines(previous, current);
    let mut lines = Vec::new();
    let mut added = 0;
    let mut removed = 0;

    for group in diff.grouped_ops(2) {
        for operation in group {
            for change in diff.iter_changes(&operation) {
                let kind = match change.tag() {
                    ChangeTag::Delete => {
                        removed += 1;
                        DiffKind::Remove
                    }
                    ChangeTag::Insert => {
                        added += 1;
                        DiffKind::Add
                    }
                    ChangeTag::Equal => DiffKind::Equal,
                };
                if lines.len() < MAX_DIFF_LINES {
                    lines.push(DiffLine {
                        kind,
                        old_number: change.old_index().map(|index| index + 1),
                        new_number: change.new_index().map(|index| index + 1),
                        text: change.value().trim_end_matches('\n').to_string(),
                    });
                }
            }
        }
    }

    DiffResult {
        lines,
        added,
        removed,
        detail: (added + removed > MAX_DIFF_LINES).then(|| "diff preview truncated".into()),
    }
}

fn seed_snapshots(root: &Path, snapshots: &mut HashMap<PathBuf, Snapshot>) {
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !ignored_entry(entry))
        .filter_map(Result::ok)
    {
        snapshots.insert(entry.path().to_path_buf(), read_snapshot(entry.path()));
    }
}

fn ignored_entry(entry: &DirEntry) -> bool {
    entry.depth() > 0 && ignored(entry.path())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
        time::{Duration, Instant, SystemTime},
    };

    use notify::Error;

    use super::*;

    #[test]
    fn creates_compact_contextual_diff() {
        let diff = build_diff(
            Some("alpha\nbeta\ngamma\n"),
            Some("alpha\nbetter\ngamma\ndelta\n"),
        );
        assert_eq!(diff.added, 2);
        assert_eq!(diff.removed, 1);
        assert!(diff.lines.iter().any(|line| line.kind == DiffKind::Add));
        assert!(diff.lines.iter().any(|line| line.kind == DiffKind::Remove));
    }

    #[test]
    fn ignores_only_heavy_generated_trees() {
        assert!(ignored(Path::new("project/.git/index")));
        assert!(ignored(Path::new("project/target/debug/app")));
        assert!(!ignored(Path::new("project/.github/workflows/check.yml")));
        assert!(!ignored(Path::new("project/src/main.rs")));
    }

    #[test]
    fn maps_overflow_errors_to_uncertain_integrity() {
        let event = integrity_from_notify_error(
            "filesystem",
            Error::generic("event queue overflow; rescan required"),
            Path::new("/workspace"),
        );
        assert_eq!(event.level, IntegrityLevel::Uncertain);
        assert_eq!(event.summary, "native events may have been lost");
    }

    #[test]
    fn labels_same_named_roots_with_shortest_unique_suffix() {
        let roots = vec![
            PathBuf::from("/work/team/api"),
            PathBuf::from("/work/other/api"),
            PathBuf::from("/work/team/web"),
        ];
        let labels = build_root_labels(&roots);
        assert_eq!(labels[&roots[0]], "team/api");
        assert_eq!(labels[&roots[1]], "other/api");
        assert_eq!(labels[&roots[2]], "web");
    }

    #[test]
    fn overlapping_roots_assign_events_to_the_deepest_workspace() {
        let parent = PathBuf::from("/work/project");
        let child = parent.join("packages/api");
        let state =
            WatchState::start(vec![temporary_directory("parent")]).expect("temporary watch state");
        let mut state = state;
        state.roots = vec![parent, child.clone()];
        assert_eq!(state.root_for(&child.join("src/main.rs")), child);
    }

    #[test]
    fn receives_native_events_from_multiple_roots_concurrently() {
        let first = temporary_directory("multi-first")
            .canonicalize()
            .expect("canonical first root");
        let second = temporary_directory("multi-second")
            .canonicalize()
            .expect("canonical second root");
        let state = WatchState::start(vec![first.clone(), second.clone()]).expect("watch state");
        let barrier = Arc::new(Barrier::new(3));

        let first_writer = spawn_writer(barrier.clone(), first.join("first.txt"));
        let second_writer = spawn_writer(barrier.clone(), second.join("second.txt"));
        barrier.wait();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_first = false;
        let mut saw_second = false;
        while Instant::now() < deadline && !(saw_first && saw_second) {
            let Ok(result) = state.receiver.recv_timeout(Duration::from_millis(250)) else {
                continue;
            };
            let event = result.expect("native event");
            saw_first |= event.paths.iter().any(|path| path.starts_with(&first));
            saw_second |= event.paths.iter().any(|path| path.starts_with(&second));
        }

        first_writer.join().expect("first writer");
        second_writer.join().expect("second writer");
        assert!(saw_first, "first root did not produce a native event");
        assert!(saw_second, "second root did not produce a native event");
        let _ = fs::remove_dir_all(first);
        let _ = fs::remove_dir_all(second);
    }

    #[test]
    fn dynamically_added_root_receives_native_events() {
        let first = temporary_directory("dynamic-first")
            .canonicalize()
            .expect("canonical first root");
        let second = temporary_directory("dynamic-second")
            .canonicalize()
            .expect("canonical second root");
        let mut state = WatchState::start(vec![first.clone()]).expect("watch state");
        assert!(
            state
                .add_root(second.clone())
                .expect("add related root")
                .is_some()
        );

        fs::write(second.join("joined.txt"), "joined dynamically\n").expect("write joined file");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = false;
        while Instant::now() < deadline && !observed {
            let Ok(result) = state.receiver.recv_timeout(Duration::from_millis(250)) else {
                continue;
            };
            observed = result
                .expect("native event")
                .paths
                .iter()
                .any(|path| path.starts_with(&second));
        }

        assert!(observed, "dynamically added root produced no native event");
        let _ = fs::remove_dir_all(first);
        let _ = fs::remove_dir_all(second);
    }

    fn spawn_writer(barrier: Arc<Barrier>, path: PathBuf) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            barrier.wait();
            fs::write(path, "native event\n").expect("write watched file");
        })
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("flux-{label}-{unique}"));
        fs::create_dir_all(&path).expect("temporary directory");
        path
    }
}
