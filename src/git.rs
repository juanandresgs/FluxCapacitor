use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, TryRecvError, TrySendError},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{
    model::{IntegrityEvent, IntegrityLevel},
    watcher::integrity_from_notify_error,
};

const EVENT_QUIET_PERIOD: Duration = Duration::from_millis(120);
const GIT_QUEUE_CAPACITY: usize = 4096;
const MAX_GIT_EVENTS_PER_DRAIN: usize = 1024;
const RECOVERY_INITIAL_DELAY: Duration = Duration::from_millis(250);
const RECOVERY_MAX_DELAY: Duration = Duration::from_secs(10);

struct RecoveryState {
    attempts: u32,
    next_attempt: Instant,
}

#[derive(Clone, Debug)]
struct RepoSnapshot {
    head: Option<String>,
    branch: Option<String>,
    subject: Option<String>,
    parent_count: usize,
    branches: HashMap<String, String>,
    tags: HashMap<String, String>,
    remotes: HashMap<String, String>,
    stash: Option<String>,
    operation: Option<GitOperation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitOperation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

impl GitOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
            Self::Bisect => "bisect",
        }
    }
}

#[derive(Clone, Debug)]
struct Repository {
    root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    snapshot: RepoSnapshot,
    dirty_since: Option<Instant>,
}

#[derive(Clone, Debug)]
pub struct GitActivity {
    pub root: PathBuf,
    pub summary: String,
    pub detail: String,
}

pub struct GitMonitor {
    repositories: Vec<Repository>,
    watched_metadata: HashSet<PathBuf>,
    known_worktrees: HashSet<PathBuf>,
    worktrees_dirty_since: HashMap<PathBuf, Instant>,
    receiver: Receiver<notify::Result<Event>>,
    dropped_events: Arc<AtomicU64>,
    failed_metadata: HashMap<PathBuf, RecoveryState>,
    _watcher: RecommendedWatcher,
    disconnected_reported: bool,
}

pub enum GitMonitorEvent {
    Activity(GitActivity),
    Integrity(IntegrityEvent),
    RelatedWorktree(PathBuf),
}

impl GitMonitor {
    pub fn discover(roots: &[PathBuf]) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(GIT_QUEUE_CAPACITY);
        let dropped_events = Arc::new(AtomicU64::new(0));
        let callback_drops = Arc::clone(&dropped_events);
        let mut watcher = notify::recommended_watcher(move |result| {
            if let Err(TrySendError::Full(_)) = sender.try_send(result) {
                callback_drops.fetch_add(1, Ordering::Relaxed);
            }
        })
        .context("failed to create Git metadata watcher")?;
        let mut seen = HashSet::new();
        let mut watched_metadata = HashSet::new();
        let mut repositories = Vec::new();

        for root in roots {
            let Some(repository_root) = git_output(root, &["rev-parse", "--show-toplevel"])
                .and_then(|output| successful_text(&output))
                .map(PathBuf::from)
            else {
                continue;
            };
            if seen.insert(repository_root.clone()) {
                let Some(git_dir) =
                    command_text(&repository_root, &["rev-parse", "--absolute-git-dir"])
                        .map(PathBuf::from)
                else {
                    continue;
                };
                let common_dir =
                    git_common_dir(&repository_root).unwrap_or_else(|| git_dir.clone());
                if watched_metadata.insert(git_dir.clone()) {
                    watcher
                        .watch(&git_dir, RecursiveMode::Recursive)
                        .with_context(|| {
                            format!("failed to watch Git metadata at {}", git_dir.display())
                        })?;
                }
                if watched_metadata.insert(common_dir.clone()) {
                    watcher
                        .watch(&common_dir, RecursiveMode::Recursive)
                        .with_context(|| {
                            format!(
                                "failed to watch shared Git metadata at {}",
                                common_dir.display()
                            )
                        })?;
                }
                repositories.push(Repository {
                    snapshot: read_snapshot(&repository_root, &git_dir),
                    root: repository_root,
                    git_dir,
                    common_dir,
                    dirty_since: None,
                });
            }
        }

        Ok(Self {
            known_worktrees: related_worktrees(roots).into_iter().collect(),
            watched_metadata,
            worktrees_dirty_since: HashMap::new(),
            repositories,
            receiver,
            dropped_events,
            failed_metadata: HashMap::new(),
            _watcher: watcher,
            disconnected_reported: false,
        })
    }

    pub fn drain(&mut self) -> Vec<GitMonitorEvent> {
        let mut output = Vec::new();
        let dropped = self.dropped_events.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            output.push(GitMonitorEvent::Integrity(IntegrityEvent {
                level: IntegrityLevel::Uncertain,
                source: "git",
                summary: "Git event queue overflowed".into(),
                detail: format!("{dropped} native metadata events were dropped"),
                root: PathBuf::new(),
            }));
        }
        let mut processed = 0;
        loop {
            if processed >= MAX_GIT_EVENTS_PER_DRAIN {
                break;
            }
            match self.receiver.try_recv() {
                Ok(Ok(event)) => {
                    processed += 1;
                    if event.need_rescan() {
                        output.push(GitMonitorEvent::Integrity(IntegrityEvent {
                            level: IntegrityLevel::Uncertain,
                            source: "git",
                            summary: "native Git events may have been lost".into(),
                            detail: event
                                .info()
                                .unwrap_or("the operating system requested a reconciliation scan")
                                .into(),
                            root: self
                                .repositories
                                .first()
                                .map(|repository| repository.root.clone())
                                .unwrap_or_default(),
                        }));
                        continue;
                    }
                    for repository in &mut self.repositories {
                        if event.paths.is_empty()
                            || event.paths.iter().any(|path| {
                                path.starts_with(&repository.git_dir)
                                    || path.starts_with(&repository.common_dir)
                            })
                        {
                            repository.dirty_since = Some(Instant::now());
                        }
                    }
                    for common_dir in self
                        .repositories
                        .iter()
                        .map(|repository| &repository.common_dir)
                    {
                        if event.paths.is_empty()
                            || event.paths.iter().any(|path| path.starts_with(common_dir))
                        {
                            self.worktrees_dirty_since
                                .insert(common_dir.clone(), Instant::now());
                        }
                    }
                }
                Ok(Err(error)) => {
                    processed += 1;
                    let affected = error.paths.clone();
                    let fallback = self
                        .repositories
                        .first()
                        .map(|repository| repository.root.as_path())
                        .unwrap_or_else(|| Path::new("."));
                    let mut integrity = integrity_from_notify_error("git", error, fallback);
                    if let Some(repository) = self.repositories.iter().find(|repository| {
                        integrity.root.starts_with(&repository.git_dir)
                            || integrity.root.starts_with(&repository.common_dir)
                    }) {
                        integrity.root = repository.root.clone();
                    }
                    output.push(GitMonitorEvent::Integrity(integrity));
                    for metadata in self
                        .watched_metadata
                        .iter()
                        .filter(|metadata| {
                            affected.is_empty()
                                || affected.iter().any(|path| {
                                    path.starts_with(metadata) || metadata.starts_with(path)
                                })
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        self.schedule_metadata_recovery(metadata);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.disconnected_reported {
                        self.disconnected_reported = true;
                        output.push(GitMonitorEvent::Integrity(IntegrityEvent {
                            level: IntegrityLevel::Lost,
                            source: "git",
                            summary: "event channel disconnected".into(),
                            detail: "Git metadata changes are no longer observable".into(),
                            root: self
                                .repositories
                                .first()
                                .map(|repository| repository.root.clone())
                                .unwrap_or_default(),
                        }));
                    }
                    break;
                }
            }
        }

        output.extend(self.recover_metadata());

        let mut activities = Vec::new();
        for repository in &mut self.repositories {
            if repository
                .dirty_since
                .is_none_or(|changed| changed.elapsed() < EVENT_QUIET_PERIOD)
            {
                continue;
            }
            repository.dirty_since = None;
            let current = read_snapshot(&repository.root, &repository.git_dir);
            classify_changes(
                &repository.root,
                &repository.snapshot,
                &current,
                &mut activities,
            );
            repository.snapshot = current;
        }
        output.extend(activities.into_iter().map(GitMonitorEvent::Activity));
        let ready = self
            .worktrees_dirty_since
            .iter()
            .filter(|(_, changed)| changed.elapsed() >= EVENT_QUIET_PERIOD)
            .map(|(common_dir, _)| common_dir.clone())
            .collect::<Vec<_>>();
        for common_dir in ready {
            self.worktrees_dirty_since.remove(&common_dir);
            let Some(root) = self
                .repositories
                .iter()
                .find(|repository| repository.common_dir == common_dir)
                .map(|repository| repository.root.clone())
            else {
                continue;
            };
            for worktree in worktrees_for_root(&root) {
                if self.known_worktrees.insert(worktree.clone()) {
                    output.push(GitMonitorEvent::RelatedWorktree(worktree));
                }
            }
        }
        output
    }

    fn schedule_metadata_recovery(&mut self, path: PathBuf) {
        let _ = self._watcher.unwatch(&path);
        self.watched_metadata.remove(&path);
        self.failed_metadata.entry(path).or_insert(RecoveryState {
            attempts: 0,
            next_attempt: Instant::now() + RECOVERY_INITIAL_DELAY,
        });
    }

    fn recover_metadata(&mut self) -> Vec<GitMonitorEvent> {
        let now = Instant::now();
        let due = self
            .failed_metadata
            .iter()
            .filter(|(_, state)| state.next_attempt <= now)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        for path in due {
            let result = path
                .is_dir()
                .then(|| self._watcher.watch(&path, RecursiveMode::Recursive))
                .transpose();
            if matches!(result, Ok(Some(()))) {
                self.failed_metadata.remove(&path);
                self.watched_metadata.insert(path.clone());
                let repository = self
                    .repositories
                    .iter_mut()
                    .find(|repository| path == repository.git_dir || path == repository.common_dir);
                let root = if let Some(repository) = repository {
                    repository.snapshot = read_snapshot(&repository.root, &repository.git_dir);
                    repository.root.clone()
                } else {
                    PathBuf::new()
                };
                output.push(GitMonitorEvent::Integrity(IntegrityEvent {
                    level: IntegrityLevel::Info,
                    source: "git",
                    summary: "Git metadata watch recovered".into(),
                    detail:
                        "native observation resumed; events during the gap were not reconstructed"
                            .into(),
                    root,
                }));
                continue;
            }
            if let Some(state) = self.failed_metadata.get_mut(&path) {
                state.attempts = state.attempts.saturating_add(1);
                let exponent = state.attempts.min(5);
                let delay = RECOVERY_INITIAL_DELAY
                    .saturating_mul(2_u32.saturating_pow(exponent))
                    .min(RECOVERY_MAX_DELAY);
                state.next_attempt = now + delay;
            }
        }
        output
    }

    pub fn add_root(&mut self, root: &Path) -> Vec<GitMonitorEvent> {
        let mut output = Vec::new();
        let Some(repository_root) = git_output(root, &["rev-parse", "--show-toplevel"])
            .and_then(|result| successful_text(&result))
            .map(PathBuf::from)
        else {
            return output;
        };
        if self
            .repositories
            .iter()
            .any(|repository| repository.root == repository_root)
        {
            return output;
        }
        let Some(git_dir) =
            command_text(&repository_root, &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
        else {
            return output;
        };
        let common_dir = git_common_dir(&repository_root).unwrap_or_else(|| git_dir.clone());
        for metadata in [&git_dir, &common_dir] {
            if self.watched_metadata.insert(metadata.clone())
                && let Err(error) = self._watcher.watch(metadata, RecursiveMode::Recursive)
            {
                self.watched_metadata.remove(metadata);
                output.push(GitMonitorEvent::Integrity(integrity_from_notify_error(
                    "git",
                    error,
                    &repository_root,
                )));
                return output;
            }
        }
        self.repositories.push(Repository {
            snapshot: read_snapshot(&repository_root, &git_dir),
            root: repository_root.clone(),
            git_dir,
            common_dir,
            dirty_since: None,
        });
        output.push(GitMonitorEvent::Activity(GitActivity {
            root: repository_root,
            summary: "linked worktree joined".into(),
            detail: "discovered from native Git worktree metadata".into(),
        }));
        output
    }

    pub fn observe_workspace_event(&mut self, event: &Event) -> Vec<GitMonitorEvent> {
        let mut output = Vec::new();
        for candidate in event
            .paths
            .iter()
            .filter_map(|path| worktree_from_git_path(path))
        {
            if self
                .repositories
                .iter()
                .any(|repository| repository.root == candidate)
            {
                continue;
            }
            let Some(repository_root) = git_output(&candidate, &["rev-parse", "--show-toplevel"])
                .and_then(|result| successful_text(&result))
                .map(PathBuf::from)
            else {
                continue;
            };
            if self
                .repositories
                .iter()
                .any(|repository| repository.root == repository_root)
            {
                continue;
            }
            let Some(git_dir) =
                command_text(&repository_root, &["rev-parse", "--absolute-git-dir"])
                    .map(PathBuf::from)
            else {
                continue;
            };
            let common_dir = git_common_dir(&repository_root).unwrap_or_else(|| git_dir.clone());
            match self._watcher.watch(&git_dir, RecursiveMode::Recursive) {
                Ok(()) => {
                    if common_dir != git_dir
                        && let Err(error) =
                            self._watcher.watch(&common_dir, RecursiveMode::Recursive)
                    {
                        output.push(GitMonitorEvent::Integrity(integrity_from_notify_error(
                            "git",
                            error,
                            &repository_root,
                        )));
                        continue;
                    }
                    self.repositories.push(Repository {
                        snapshot: read_snapshot(&repository_root, &git_dir),
                        root: repository_root.clone(),
                        git_dir: git_dir.clone(),
                        common_dir,
                        dirty_since: None,
                    });
                    output.push(GitMonitorEvent::Activity(GitActivity {
                        root: repository_root,
                        summary: "repository initialized".into(),
                        detail: format!("watching {}", git_dir.display()),
                    }));
                }
                Err(error) => output.push(GitMonitorEvent::Integrity(integrity_from_notify_error(
                    "git",
                    error,
                    &repository_root,
                ))),
            }
        }
        output
    }

    pub fn repository_count(&self) -> usize {
        self.repositories.len()
    }

    pub fn established_events(&self) -> Vec<IntegrityEvent> {
        self.repositories
            .iter()
            .map(|repository| IntegrityEvent {
                level: IntegrityLevel::Info,
                source: "git",
                summary: "metadata watch established".into(),
                detail: format!("watching {}", repository.git_dir.display()),
                root: repository.root.clone(),
            })
            .collect()
    }
}

pub fn related_worktrees(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut worktrees = Vec::new();
    for root in roots {
        for worktree in worktrees_for_root(root) {
            if !worktrees.contains(&worktree) {
                worktrees.push(worktree);
            }
        }
    }
    worktrees
}

fn worktrees_for_root(root: &Path) -> Vec<PathBuf> {
    command_text(root, &["worktree", "list", "--porcelain"])
        .map(|output| {
            output
                .lines()
                .filter_map(|line| line.strip_prefix("worktree "))
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
                .filter_map(|path| {
                    command_text(&path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
                })
                .map(|path| path.canonicalize().unwrap_or(path))
                .collect()
        })
        .unwrap_or_default()
}

fn worktree_from_git_path(path: &Path) -> Option<PathBuf> {
    let mut worktree = PathBuf::new();
    for component in path.components() {
        if component.as_os_str() == ".git" {
            return Some(worktree);
        }
        worktree.push(component.as_os_str());
    }
    None
}

fn git_common_dir(root: &Path) -> Option<PathBuf> {
    command_text(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(PathBuf::from)
}

fn read_snapshot(root: &Path, git_dir: &Path) -> RepoSnapshot {
    let head = command_text(root, &["rev-parse", "--verify", "HEAD"]);
    let branch = command_text(root, &["symbolic-ref", "--short", "-q", "HEAD"]);
    let (subject, parent_count) = command_text(root, &["show", "-s", "--format=%s%x00%P", "HEAD"])
        .and_then(|value| {
            let (subject, parents) = value.split_once('\0')?;
            Some((
                subject.to_string(),
                parents
                    .split_whitespace()
                    .filter(|value| !value.is_empty())
                    .count(),
            ))
        })
        .map_or((None, 0), |(subject, count)| (Some(subject), count));

    let branches = read_refs(root, "refs/heads");
    let tags = read_refs(root, "refs/tags");
    let remotes = read_refs(root, "refs/remotes")
        .into_iter()
        .filter(|(name, _)| !name.ends_with("/HEAD"))
        .collect();
    let stash = command_text(root, &["rev-parse", "--verify", "refs/stash"]);
    let operation = read_operation(git_dir);

    RepoSnapshot {
        head,
        branch,
        subject,
        parent_count,
        branches,
        tags,
        remotes,
        stash,
        operation,
    }
}

fn read_refs(root: &Path, namespace: &str) -> HashMap<String, String> {
    command_text(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)",
            namespace,
        ],
    )
    .map(|output| {
        output
            .lines()
            .filter_map(|line| {
                let (name, object) = line.split_once('\0')?;
                Some((name.to_string(), object.to_string()))
            })
            .collect()
    })
    .unwrap_or_default()
}

fn read_operation(git_dir: &Path) -> Option<GitOperation> {
    if git_dir.join("MERGE_HEAD").exists() {
        Some(GitOperation::Merge)
    } else if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        Some(GitOperation::Rebase)
    } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
        Some(GitOperation::CherryPick)
    } else if git_dir.join("REVERT_HEAD").exists() {
        Some(GitOperation::Revert)
    } else if git_dir.join("BISECT_START").exists() {
        Some(GitOperation::Bisect)
    } else {
        None
    }
}

fn classify_changes(
    root: &Path,
    previous: &RepoSnapshot,
    current: &RepoSnapshot,
    activities: &mut Vec<GitActivity>,
) {
    match (previous.operation, current.operation) {
        (None, Some(operation)) => activities.push(activity(
            root,
            format!("{} started", operation.label()),
            "Git operation state appeared".into(),
        )),
        (Some(operation), None) => activities.push(activity(
            root,
            format!("{} ended", operation.label()),
            "Git operation state disappeared".into(),
        )),
        (Some(previous), Some(current)) if previous != current => {
            activities.push(activity(
                root,
                format!("{} ended", previous.label()),
                "Git operation state changed".into(),
            ));
            activities.push(activity(
                root,
                format!("{} started", current.label()),
                "Git operation state changed".into(),
            ));
        }
        _ => {}
    }

    if previous.branch != current.branch {
        let from = previous.branch.as_deref().unwrap_or("detached HEAD");
        let to = current.branch.as_deref().unwrap_or("detached HEAD");
        activities.push(activity(
            root,
            format!("checkout {to}"),
            format!("{from} → {to}"),
        ));
    }

    if previous.head != current.head {
        let short = current.head.as_deref().map(short_oid).unwrap_or("unborn");
        let subject = current.subject.as_deref().unwrap_or("HEAD removed");
        if current.head.is_none() {
            activities.push(activity(
                root,
                "HEAD deleted".into(),
                "repository has no commits".into(),
            ));
        } else if current.parent_count > 1 {
            activities.push(activity(root, format!("merge {short}"), subject.into()));
        } else if previous.branch == current.branch {
            let label = classify_head_move(root, previous.head.as_deref(), current.head.as_deref());
            activities.push(activity(root, format!("{label} {short}"), subject.into()));
        } else if previous.head.is_none() {
            activities.push(activity(root, format!("commit {short}"), subject.into()));
        }
    }

    for branch in current.branches.keys() {
        if !previous.branches.contains_key(branch) && Some(branch) != current.branch.as_ref() {
            activities.push(activity(
                root,
                format!("branch created: {branch}"),
                "local branch".into(),
            ));
        }
    }
    for branch in previous.branches.keys() {
        if !current.branches.contains_key(branch) {
            activities.push(activity(
                root,
                format!("branch deleted: {branch}"),
                "local branch".into(),
            ));
        }
    }

    for (branch, object) in &current.branches {
        if previous
            .branches
            .get(branch)
            .is_some_and(|previous_object| previous_object != object)
            && Some(branch) != current.branch.as_ref()
        {
            activities.push(activity(
                root,
                format!("branch updated: {branch}"),
                format!("now at {}", short_oid(object)),
            ));
        }
    }

    classify_ref_changes(root, "tag", &previous.tags, &current.tags, activities);
    classify_ref_changes(
        root,
        "remote ref",
        &previous.remotes,
        &current.remotes,
        activities,
    );

    match (&previous.stash, &current.stash) {
        (None, Some(object)) => activities.push(activity(
            root,
            "stash created".into(),
            format!("refs/stash at {}", short_oid(object)),
        )),
        (Some(_), None) => activities.push(activity(
            root,
            "stash deleted".into(),
            "refs/stash removed".into(),
        )),
        (Some(previous), Some(current)) if previous != current => activities.push(activity(
            root,
            "stash updated".into(),
            format!("refs/stash now at {}", short_oid(current)),
        )),
        _ => {}
    }
}

fn classify_ref_changes(
    root: &Path,
    label: &str,
    previous: &HashMap<String, String>,
    current: &HashMap<String, String>,
    activities: &mut Vec<GitActivity>,
) {
    for (name, object) in current {
        match previous.get(name) {
            None => activities.push(activity(
                root,
                format!("{label} created: {name}"),
                format!("points to {}", short_oid(object)),
            )),
            Some(previous_object) if previous_object != object => activities.push(activity(
                root,
                format!("{label} updated: {name}"),
                format!("now at {}", short_oid(object)),
            )),
            _ => {}
        }
    }
    for name in previous.keys() {
        if !current.contains_key(name) {
            activities.push(activity(
                root,
                format!("{label} deleted: {name}"),
                "reference removed".into(),
            ));
        }
    }
}

fn classify_head_move(root: &Path, previous: Option<&str>, current: Option<&str>) -> &'static str {
    let (Some(previous), Some(current)) = (previous, current) else {
        return "commit";
    };
    if command_success(root, &["merge-base", "--is-ancestor", previous, current]) {
        "commit"
    } else if command_success(root, &["merge-base", "--is-ancestor", current, previous]) {
        "reset"
    } else {
        "rewrite"
    }
}

fn activity(root: &Path, summary: String, detail: String) -> GitActivity {
    GitActivity {
        root: root.to_path_buf(),
        summary,
        detail,
    }
}

fn short_oid(value: &str) -> &str {
    value.get(..7).unwrap_or(value)
}

fn command_text(root: &Path, arguments: &[&str]) -> Option<String> {
    git_output(root, arguments).and_then(|output| successful_text(&output))
}

fn command_success(root: &Path, arguments: &[&str]) -> bool {
    git_output(root, arguments).is_some_and(|output| output.status.success())
}

fn git_output(root: &Path, arguments: &[&str]) -> Option<Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .ok()
}

fn successful_text(output: &Output) -> Option<String> {
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::SystemTime};

    use super::*;

    #[test]
    fn shortens_object_ids() {
        assert_eq!(short_oid("1234567890abcdef"), "1234567");
        assert_eq!(short_oid("short"), "short");
    }

    #[test]
    fn finds_worktree_from_git_metadata_path() {
        assert_eq!(
            worktree_from_git_path(Path::new("/code/project/.git/HEAD")),
            Some(PathBuf::from("/code/project"))
        );
        assert_eq!(
            worktree_from_git_path(Path::new("/code/project/src/main.rs")),
            None
        );
    }

    #[test]
    fn classifies_refs_stash_and_operation_state() {
        let root = Path::new("/workspace");
        let mut previous = empty_snapshot();
        let mut current = empty_snapshot();
        current.operation = Some(GitOperation::Rebase);
        current.tags.insert("v1.0.0".into(), "aaaaaaaa".into());
        current
            .remotes
            .insert("origin/main".into(), "bbbbbbbb".into());
        current.stash = Some("cccccccc".into());

        let mut activities = Vec::new();
        classify_changes(root, &previous, &current, &mut activities);
        let summaries = activities
            .iter()
            .map(|activity| activity.summary.as_str())
            .collect::<Vec<_>>();
        assert!(summaries.contains(&"rebase started"));
        assert!(summaries.contains(&"tag created: v1.0.0"));
        assert!(summaries.contains(&"remote ref created: origin/main"));
        assert!(summaries.contains(&"stash created"));

        previous = current;
        let mut ended = previous.clone();
        ended.operation = None;
        ended.tags.clear();
        ended.remotes.clear();
        ended.stash = None;
        activities.clear();
        classify_changes(root, &previous, &ended, &mut activities);
        let summaries = activities
            .iter()
            .map(|activity| activity.summary.as_str())
            .collect::<Vec<_>>();
        assert!(summaries.contains(&"rebase ended"));
        assert!(summaries.contains(&"tag deleted: v1.0.0"));
        assert!(summaries.contains(&"remote ref deleted: origin/main"));
        assert!(summaries.contains(&"stash deleted"));
    }

    #[test]
    fn native_git_event_emits_commit_activity() {
        let root = temporary_directory();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Flux Test"]);
        git(&root, &["config", "user.email", "flux@example.invalid"]);
        fs::write(root.join("story.txt"), "first\n").expect("write initial file");
        git(&root, &["add", "story.txt"]);
        git(&root, &["commit", "-qm", "initial"]);

        let mut monitor = GitMonitor::discover(std::slice::from_ref(&root)).expect("Git monitor");
        fs::write(root.join("story.txt"), "first\nsecond\n").expect("write changed file");
        git(&root, &["add", "story.txt"]);
        git(&root, &["commit", "-qm", "second commit"]);
        git(&root, &["tag", "v-test"]);
        git(&root, &["branch", "side"]);

        let deadline = Instant::now() + Duration::from_secs(4);
        let mut activities = Vec::new();
        while Instant::now() < deadline && activities.is_empty() {
            thread::sleep(Duration::from_millis(40));
            activities.extend(monitor.drain().into_iter().filter_map(|event| match event {
                GitMonitorEvent::Activity(activity) => Some(activity),
                GitMonitorEvent::Integrity(_) | GitMonitorEvent::RelatedWorktree(_) => None,
            }));
        }

        assert!(
            activities
                .iter()
                .any(|activity| activity.summary.starts_with("commit ")
                    && activity.detail == "second commit"),
            "expected commit activity, got {activities:?}"
        );
        assert!(
            activities
                .iter()
                .any(|activity| activity.summary == "tag created: v-test")
        );
        assert!(
            activities
                .iter()
                .any(|activity| activity.summary == "branch created: side")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovered_git_metadata_watch_resumes_activity() {
        let root = temporary_directory();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Flux Test"]);
        git(&root, &["config", "user.email", "flux@example.invalid"]);
        fs::write(root.join("story.txt"), "first\n").expect("write initial file");
        git(&root, &["add", "story.txt"]);
        git(&root, &["commit", "-qm", "initial"]);

        let mut monitor = GitMonitor::discover(std::slice::from_ref(&root)).expect("Git monitor");
        let git_dir = monitor.repositories[0].git_dir.clone();
        monitor.schedule_metadata_recovery(git_dir);
        thread::sleep(RECOVERY_INITIAL_DELAY + Duration::from_millis(50));
        assert!(monitor.recover_metadata().into_iter().any(|event| matches!(
            event,
            GitMonitorEvent::Integrity(IntegrityEvent {
                level: IntegrityLevel::Info,
                summary,
                ..
            }) if summary == "Git metadata watch recovered"
        )));

        fs::write(root.join("story.txt"), "first\nsecond\n").expect("write changed file");
        git(&root, &["add", "story.txt"]);
        git(&root, &["commit", "-qm", "after recovery"]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = false;
        while Instant::now() < deadline && !observed {
            thread::sleep(Duration::from_millis(40));
            observed = monitor.drain().into_iter().any(|event| matches!(
                event,
                GitMonitorEvent::Activity(GitActivity { detail, .. }) if detail == "after recovery"
            ));
        }
        assert!(observed, "recovered Git watch produced no commit activity");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repository_initialization_joins_the_native_stream() {
        let root = temporary_directory();
        let mut monitor = GitMonitor::discover(std::slice::from_ref(&root)).expect("Git monitor");
        assert_eq!(monitor.repository_count(), 0);
        git(&root, &["init", "-q"]);
        let events = monitor.observe_workspace_event(
            &Event::new(notify::EventKind::Create(notify::event::CreateKind::Folder))
                .add_path(root.join(".git")),
        );
        assert_eq!(monitor.repository_count(), 1);
        assert!(events.into_iter().any(|event| matches!(
            event,
            GitMonitorEvent::Activity(GitActivity { summary, .. })
                if summary == "repository initialized"
        )));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn linked_worktree_joins_from_native_git_metadata() {
        let root = temporary_directory();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Flux Test"]);
        git(&root, &["config", "user.email", "flux@example.invalid"]);
        fs::write(root.join("story.txt"), "initial\n").expect("write initial file");
        git(&root, &["add", "story.txt"]);
        git(&root, &["commit", "-qm", "initial"]);
        let linked = root.with_extension("linked");
        let mut monitor = GitMonitor::discover(std::slice::from_ref(&root)).expect("Git monitor");
        thread::sleep(Duration::from_millis(250));

        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature/native-related",
                linked.to_str().expect("linked path"),
            ],
        );
        let linked = linked.canonicalize().expect("canonical linked worktree");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut discovered = false;
        while Instant::now() < deadline && !discovered {
            thread::sleep(Duration::from_millis(40));
            discovered = monitor.drain().into_iter().any(
                |event| matches!(event, GitMonitorEvent::RelatedWorktree(path) if path == linked),
            );
        }

        assert!(discovered, "linked worktree was not discovered natively");
        let _ = fs::remove_dir_all(linked);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn administrative_git_path_resolves_to_working_tree() {
        let container = temporary_directory();
        let checkout = container.join("checkout");
        let admin = container.join("admin");
        fs::create_dir_all(&checkout).expect("checkout directory");
        let status = Command::new("git")
            .args(["init", "-q", "--separate-git-dir"])
            .arg(&admin)
            .arg(&checkout)
            .status()
            .expect("initialize separate git directory");
        assert!(status.success());
        git(&admin, &["config", "core.worktree", "../checkout"]);

        let checkout = checkout.canonicalize().expect("canonical checkout");
        let worktrees = related_worktrees(std::slice::from_ref(&checkout));
        assert_eq!(worktrees, vec![checkout]);
        assert!(!worktrees.contains(&admin));
        let _ = fs::remove_dir_all(container);
    }

    fn git(root: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .status()
            .expect("run git");
        assert!(status.success(), "git command failed: {arguments:?}");
    }

    fn temporary_directory() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("flux-git-events-{unique}"));
        fs::create_dir_all(&path).expect("temporary directory");
        path
    }

    fn empty_snapshot() -> RepoSnapshot {
        RepoSnapshot {
            head: None,
            branch: None,
            subject: None,
            parent_count: 0,
            branches: HashMap::new(),
            tags: HashMap::new(),
            remotes: HashMap::new(),
            stash: None,
            operation: None,
        }
    }
}
