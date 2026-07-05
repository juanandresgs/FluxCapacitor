use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, TrySendError},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use notify::{ErrorKind, Event, RecommendedWatcher, RecursiveMode, Watcher};
use similar::{ChangeTag, TextDiff};
use walkdir::{DirEntry, WalkDir};

use crate::model::{DiffKind, DiffLine, IntegrityEvent, IntegrityLevel};

const MAX_TEXT_BYTES: u64 = 1024 * 1024;
const MAX_DIFF_LINES: usize = 300;
const INITIAL_SEED_BUDGET: usize = 2048;
const FILESYSTEM_QUEUE_CAPACITY: usize = 8192;
const RECOVERY_INITIAL_DELAY: Duration = Duration::from_millis(250);
const RECOVERY_MAX_DELAY: Duration = Duration::from_secs(10);
pub const DEFAULT_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_SNAPSHOT_ENTRIES: usize = 200_000;

type SnapshotSeeder = Box<dyn Iterator<Item = walkdir::Result<DirEntry>>>;

struct RecoveryState {
    attempts: u32,
    next_attempt: Instant,
}

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
    seeders: Vec<SnapshotSeeder>,
    watched_roots: HashSet<PathBuf>,
    failed_roots: HashMap<PathBuf, RecoveryState>,
    dropped_events: Arc<AtomicU64>,
    snapshot_bytes: usize,
    max_snapshot_bytes: usize,
    max_snapshot_entries: usize,
    snapshot_omissions: u64,
    _watcher: RecommendedWatcher,
}

pub struct DiffResult {
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
    pub detail: Option<String>,
}

impl WatchState {
    #[cfg(test)]
    pub fn start(roots: Vec<PathBuf>) -> Result<Self> {
        Self::start_with_snapshot_budget(roots, DEFAULT_SNAPSHOT_BYTES)
    }

    #[cfg(test)]
    pub fn start_with_snapshot_budget(
        roots: Vec<PathBuf>,
        max_snapshot_bytes: usize,
    ) -> Result<Self> {
        Self::start_with_snapshot_limits(roots, max_snapshot_bytes, DEFAULT_SNAPSHOT_ENTRIES)
    }

    pub fn start_with_snapshot_limits(
        roots: Vec<PathBuf>,
        max_snapshot_bytes: usize,
        max_snapshot_entries: usize,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(FILESYSTEM_QUEUE_CAPACITY);
        let dropped_events = Arc::new(AtomicU64::new(0));
        let callback_drops = Arc::clone(&dropped_events);
        let mut watcher = notify::recommended_watcher(move |result| {
            if let Err(TrySendError::Full(_)) = sender.try_send(result) {
                callback_drops.fetch_add(1, Ordering::Relaxed);
            }
        })
        .context("failed to create filesystem watcher")?;

        let snapshots = HashMap::new();
        let mut seeders = Vec::new();
        let observation_roots = minimal_observation_roots(&roots);
        for root in &observation_roots {
            watcher
                .watch(root, RecursiveMode::Recursive)
                .with_context(|| format!("failed to watch {}", root.display()))?;
            seeders.push(snapshot_seeder(root));
        }

        let root_labels = build_root_labels(&roots);
        let mut state = Self {
            roots,
            root_labels,
            snapshots,
            receiver,
            seeders,
            watched_roots: observation_roots.into_iter().collect(),
            failed_roots: HashMap::new(),
            dropped_events,
            snapshot_bytes: 0,
            max_snapshot_bytes,
            max_snapshot_entries,
            snapshot_omissions: 0,
            _watcher: watcher,
        };
        state.seed_step(INITIAL_SEED_BUDGET);
        Ok(state)
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
        self.store_snapshot(path.to_path_buf(), snapshot)
    }

    pub fn remove_snapshot(&mut self, path: &Path) -> Snapshot {
        self.snapshots.remove(path).map_or_else(
            || Snapshot {
                content: None,
                size: 0,
                is_dir: path.is_dir(),
                read_error: None,
            },
            |snapshot| {
                self.snapshot_bytes = self
                    .snapshot_bytes
                    .saturating_sub(snapshot.content.as_ref().map_or(0, String::len));
                snapshot
            },
        )
    }

    pub fn remove_snapshot_tree(&mut self, path: &Path) {
        let removed_bytes = self
            .snapshots
            .iter()
            .filter(|(candidate, _)| *candidate == path || candidate.starts_with(path))
            .map(|(_, snapshot)| snapshot.content.as_ref().map_or(0, String::len))
            .sum::<usize>();
        self.snapshots
            .retain(|candidate, _| candidate != path && !candidate.starts_with(path));
        self.snapshot_bytes = self.snapshot_bytes.saturating_sub(removed_bytes);
    }

    pub fn remap_snapshot_descendants(&mut self, from: &Path, to: &Path) {
        let moved = self
            .snapshots
            .keys()
            .filter(|path| *path != from && path.starts_with(from))
            .cloned()
            .collect::<Vec<_>>();
        for old_path in moved {
            let Some(snapshot) = self.snapshots.remove(&old_path) else {
                continue;
            };
            let Ok(relative) = old_path.strip_prefix(from) else {
                continue;
            };
            if let Some(replaced) = self.snapshots.insert(to.join(relative), snapshot) {
                self.snapshot_bytes = self
                    .snapshot_bytes
                    .saturating_sub(replaced.content.as_ref().map_or(0, String::len));
            }
        }
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        let root = self.root_for(path);
        path.strip_prefix(root).is_ok_and(ignored)
    }

    pub fn take_dropped_events(&self) -> u64 {
        self.dropped_events.swap(0, Ordering::Relaxed)
    }

    pub fn take_snapshot_omissions(&mut self) -> u64 {
        std::mem::take(&mut self.snapshot_omissions)
    }

    pub fn schedule_recovery(&mut self, root: PathBuf) {
        let _ = self._watcher.unwatch(&root);
        self.watched_roots.remove(&root);
        self.remove_snapshot_tree(&root);
        self.failed_roots.entry(root).or_insert(RecoveryState {
            attempts: 0,
            next_attempt: Instant::now() + RECOVERY_INITIAL_DELAY,
        });
    }

    pub fn recover_due(&mut self) -> Vec<IntegrityEvent> {
        let now = Instant::now();
        let due = self
            .failed_roots
            .iter()
            .filter(|(_, state)| state.next_attempt <= now)
            .map(|(root, _)| root.clone())
            .collect::<Vec<_>>();
        let mut recovered = Vec::new();
        for root in due {
            let result = root
                .is_dir()
                .then(|| self._watcher.watch(&root, RecursiveMode::Recursive))
                .transpose();
            if matches!(result, Ok(Some(()))) {
                self.failed_roots.remove(&root);
                self.watched_roots.insert(root.clone());
                self.seeders.push(snapshot_seeder(&root));
                recovered.push(IntegrityEvent {
                    level: IntegrityLevel::Info,
                    source: "filesystem",
                    summary: "watch recovered".into(),
                    detail:
                        "native observation resumed; events during the gap were not reconstructed"
                            .into(),
                    root,
                });
                continue;
            }
            if let Some(state) = self.failed_roots.get_mut(&root) {
                state.attempts = state.attempts.saturating_add(1);
                let exponent = state.attempts.min(5);
                let delay = RECOVERY_INITIAL_DELAY
                    .saturating_mul(2_u32.saturating_pow(exponent))
                    .min(RECOVERY_MAX_DELAY);
                state.next_attempt = now + delay;
            }
        }
        recovered
    }

    fn store_snapshot(&mut self, path: PathBuf, mut snapshot: Snapshot) -> Snapshot {
        if let Some(previous) = self.snapshots.remove(&path) {
            self.snapshot_bytes = self
                .snapshot_bytes
                .saturating_sub(previous.content.as_ref().map_or(0, String::len));
        }
        if self.snapshots.len() >= self.max_snapshot_entries {
            snapshot.content = None;
            snapshot.read_error = Some(format!(
                "snapshot omitted because the {} entry memory limit is full",
                self.max_snapshot_entries
            ));
            self.snapshot_omissions += 1;
            return snapshot;
        }
        let content_bytes = snapshot.content.as_ref().map_or(0, String::len);
        if content_bytes > self.max_snapshot_bytes.saturating_sub(self.snapshot_bytes) {
            snapshot.content = None;
            snapshot.read_error = Some(format!(
                "snapshot content omitted because the {} byte memory budget is full",
                self.max_snapshot_bytes
            ));
            self.snapshot_omissions += 1;
        } else {
            self.snapshot_bytes += content_bytes;
        }
        self.snapshots.insert(path, snapshot.clone());
        snapshot
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
        if !self.roots.iter().any(|existing| root.starts_with(existing)) {
            self._watcher
                .watch(&root, RecursiveMode::Recursive)
                .with_context(|| format!("failed to watch related folder {}", root.display()))?;
            self.seeders.push(snapshot_seeder(&root));
        }
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

    pub fn seed_step(&mut self, budget: usize) -> usize {
        let mut processed = 0;
        while processed < budget {
            let Some(seeder) = self.seeders.last_mut() else {
                break;
            };
            match seeder.next() {
                Some(Ok(entry)) => {
                    processed += 1;
                    let path = entry.path();
                    if path.exists() && !self.snapshots.contains_key(path) {
                        self.store_snapshot(path.to_path_buf(), read_snapshot(path));
                    }
                }
                Some(Err(_)) => processed += 1,
                None => {
                    self.seeders.pop();
                }
            }
        }
        processed
    }

    pub fn is_seeding(&self) -> bool {
        !self.seeders.is_empty()
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

fn minimal_observation_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter(|root| {
            !roots
                .iter()
                .any(|candidate| candidate != *root && root.starts_with(candidate))
        })
        .cloned()
        .collect()
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

fn snapshot_seeder(root: &Path) -> SnapshotSeeder {
    let root = root.to_path_buf();
    Box::new(WalkDir::new(&root).into_iter().filter_entry(move |entry| {
        entry.depth() == 0
            || entry
                .path()
                .strip_prefix(&root)
                .is_ok_and(|relative| !ignored(relative))
    }))
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
    fn explicit_roots_are_not_ignored_by_ancestor_or_root_name() {
        let root = PathBuf::from("/work/target");
        let mut state = WatchState::start(vec![temporary_directory("ignored-root")])
            .expect("temporary watch state");
        state.roots = vec![root.clone()];
        assert!(!state.is_ignored(&root.join("src/main.rs")));
        assert!(state.is_ignored(&root.join("target/debug/app")));
    }

    #[test]
    fn remaps_and_removes_descendant_snapshots_as_a_tree() {
        let root = temporary_directory("snapshot-tree");
        let from = root.join("before");
        let to = root.join("after");
        fs::create_dir_all(from.join("nested")).expect("nested directory");
        fs::write(from.join("nested/file.txt"), "before\n").expect("file");
        let mut state = WatchState::start(vec![root.clone()]).expect("watch state");
        while state.is_seeding() {
            state.seed_step(1024);
        }

        state.remap_snapshot_descendants(&from, &to);
        assert!(
            state
                .previous_snapshot(&to.join("nested/file.txt"))
                .is_some()
        );
        assert!(
            state
                .previous_snapshot(&from.join("nested/file.txt"))
                .is_none()
        );

        state.remove_snapshot_tree(&to);
        assert!(
            state
                .previous_snapshot(&to.join("nested/file.txt"))
                .is_none()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn snapshot_budget_omits_content_and_reports_pressure() {
        let root = temporary_directory("snapshot-budget");
        let path = root.join("file.txt");
        fs::write(&path, "larger than budget\n").expect("file");
        let mut state =
            WatchState::start_with_snapshot_budget(vec![root.clone()], 4).expect("watch state");
        while state.is_seeding() {
            state.seed_step(1024);
        }
        assert!(
            state
                .previous_snapshot(&path)
                .is_some_and(|snapshot| snapshot.content.is_none())
        );
        assert!(state.take_snapshot_omissions() > 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn snapshot_entry_limit_bounds_retained_paths() {
        let root = temporary_directory("snapshot-entry-limit");
        for name in ["one.txt", "two.txt", "three.txt"] {
            fs::write(root.join(name), name).expect("file");
        }
        let mut state =
            WatchState::start_with_snapshot_limits(vec![root.clone()], DEFAULT_SNAPSHOT_BYTES, 2)
                .expect("watch state");
        while state.is_seeding() {
            state.seed_step(1024);
        }
        assert_eq!(state.snapshots.len(), 2);
        assert!(state.take_snapshot_omissions() > 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scheduled_watch_recovery_resumes_native_events() {
        let root = temporary_directory("watch-recovery")
            .canonicalize()
            .expect("canonical root");
        let mut state = WatchState::start(vec![root.clone()]).expect("watch state");
        state.schedule_recovery(root.clone());
        thread::sleep(RECOVERY_INITIAL_DELAY + Duration::from_millis(50));
        let recovered = state.recover_due();
        assert!(recovered.iter().any(|event| {
            event.level == IntegrityLevel::Info
                && event.root == root
                && event.summary == "watch recovered"
        }));

        let path = root.join("after-recovery.txt");
        fs::write(&path, "observed\n").expect("write recovered file");
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
                .any(|candidate| candidate == &path);
        }
        assert!(observed, "recovered watch produced no native event");
        let _ = fs::remove_dir_all(root);
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
    fn nested_workspaces_share_the_minimal_physical_watch_root() {
        let parent = PathBuf::from("/work/project");
        let child = parent.join("worktrees/feature");
        let sibling = PathBuf::from("/work/other");
        assert_eq!(
            minimal_observation_roots(&[child, sibling.clone(), parent.clone()]),
            vec![sibling, parent]
        );
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
