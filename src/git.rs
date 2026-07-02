use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

const EVENT_QUIET_PERIOD: Duration = Duration::from_millis(120);

#[derive(Clone, Debug)]
struct RepoSnapshot {
    head: Option<String>,
    branch: Option<String>,
    subject: Option<String>,
    parent_count: usize,
    branches: HashMap<String, String>,
}

#[derive(Clone, Debug)]
struct Repository {
    root: PathBuf,
    git_dir: PathBuf,
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
    receiver: Receiver<notify::Result<Event>>,
    _watcher: RecommendedWatcher,
}

impl GitMonitor {
    pub fn discover(roots: &[PathBuf]) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |result| {
            let _ = sender.send(result);
        })
        .context("failed to create Git metadata watcher")?;
        let mut seen = HashSet::new();
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
                watcher
                    .watch(&git_dir, RecursiveMode::Recursive)
                    .with_context(|| {
                        format!("failed to watch Git metadata at {}", git_dir.display())
                    })?;
                repositories.push(Repository {
                    snapshot: read_snapshot(&repository_root),
                    root: repository_root,
                    git_dir,
                    dirty_since: None,
                });
            }
        }

        Ok(Self {
            repositories,
            receiver,
            _watcher: watcher,
        })
    }

    pub fn drain(&mut self) -> Vec<GitActivity> {
        while let Ok(result) = self.receiver.try_recv() {
            let Ok(event) = result else {
                continue;
            };
            for repository in &mut self.repositories {
                if event.paths.is_empty()
                    || event
                        .paths
                        .iter()
                        .any(|path| path.starts_with(&repository.git_dir))
                {
                    repository.dirty_since = Some(Instant::now());
                }
            }
        }

        let mut activities = Vec::new();
        for repository in &mut self.repositories {
            if repository
                .dirty_since
                .is_none_or(|changed| changed.elapsed() < EVENT_QUIET_PERIOD)
            {
                continue;
            }
            repository.dirty_since = None;
            let current = read_snapshot(&repository.root);
            classify_changes(
                &repository.root,
                &repository.snapshot,
                &current,
                &mut activities,
            );
            repository.snapshot = current;
        }
        activities
    }

    pub fn repository_count(&self) -> usize {
        self.repositories.len()
    }
}

fn read_snapshot(root: &Path) -> RepoSnapshot {
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

    let branches = command_text(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)",
            "refs/heads",
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
    .unwrap_or_default();

    RepoSnapshot {
        head,
        branch,
        subject,
        parent_count,
        branches,
    }
}

fn classify_changes(
    root: &Path,
    previous: &RepoSnapshot,
    current: &RepoSnapshot,
    activities: &mut Vec<GitActivity>,
) {
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

        let deadline = Instant::now() + Duration::from_secs(4);
        let mut activities = Vec::new();
        while Instant::now() < deadline && activities.is_empty() {
            thread::sleep(Duration::from_millis(40));
            activities.extend(monitor.drain());
        }

        assert!(
            activities
                .iter()
                .any(|activity| activity.summary.starts_with("commit ")
                    && activity.detail == "second commit"),
            "expected commit activity, got {activities:?}"
        );
        let _ = fs::remove_dir_all(root);
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
}
