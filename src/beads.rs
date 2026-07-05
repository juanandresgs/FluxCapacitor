use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use notify::Event;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::model::{IntegrityEvent, IntegrityLevel};

const EVENT_QUIET_PERIOD: Duration = Duration::from_millis(150);
const REQUIRED_TABLES: [&str; 5] = ["events", "issues", "comments", "dependencies", "labels"];

#[derive(Clone, Debug)]
pub struct BeadsActivity {
    pub root: PathBuf,
    pub issue_id: String,
    pub title: String,
    pub action: String,
    pub detail: String,
}

pub enum BeadsMonitorEvent {
    Activity(BeadsActivity),
    Integrity(IntegrityEvent),
}

#[derive(Debug)]
struct BeadsStore {
    root: PathBuf,
    data_dir: PathBuf,
    cursor: String,
    dirty_since: Option<Instant>,
}

pub struct BeadsMonitor {
    roots: Vec<PathBuf>,
    stores: Vec<BeadsStore>,
    established: Vec<IntegrityEvent>,
}

#[derive(Deserialize)]
struct BeadsMetadata {
    database: Option<String>,
    dolt_database: Option<String>,
    dolt_mode: Option<String>,
}

#[derive(Deserialize)]
struct JsonRows {
    #[serde(default)]
    rows: Vec<Map<String, Value>>,
}

impl BeadsMonitor {
    pub fn discover(roots: &[PathBuf]) -> Self {
        let mut monitor = Self {
            roots: roots.to_vec(),
            stores: Vec::new(),
            established: Vec::new(),
        };
        for root in roots {
            monitor.discover_root(root);
        }
        monitor
    }

    pub fn store_count(&self) -> usize {
        self.stores.len()
    }

    pub fn established_events(&self) -> Vec<IntegrityEvent> {
        self.established.clone()
    }

    pub fn add_root(&mut self, root: &Path) -> Vec<IntegrityEvent> {
        if !self.roots.iter().any(|known| known == root) {
            self.roots.push(root.to_path_buf());
        }
        let before = self.established.len();
        self.discover_root(root);
        self.established[before..].to_vec()
    }

    pub fn observe_workspace_event(&mut self, event: &Event) {
        let observed_at = Instant::now();
        for store in &mut self.stores {
            if event.paths.iter().any(|path| store.observes_path(path)) {
                store.dirty_since = Some(observed_at);
            }
        }

        let roots = self.roots.clone();
        for root in roots {
            let metadata = root.join(".beads/metadata.json");
            if event.paths.iter().any(|path| path == &metadata)
                && !self.stores.iter().any(|store| store.root == root)
            {
                self.discover_root(&root);
            }
        }
    }

    pub fn drain(&mut self) -> Vec<BeadsMonitorEvent> {
        let mut output = Vec::new();
        for store in &mut self.stores {
            if store
                .dirty_since
                .is_none_or(|dirty_since| dirty_since.elapsed() < EVENT_QUIET_PERIOD)
            {
                continue;
            }
            store.dirty_since = None;
            match advance_store(store) {
                Ok(activities) => {
                    output.extend(activities.into_iter().map(BeadsMonitorEvent::Activity))
                }
                Err(error) => output.push(BeadsMonitorEvent::Integrity(IntegrityEvent {
                    level: IntegrityLevel::Uncertain,
                    source: "beads",
                    summary: "work history could not advance".into(),
                    detail: error.to_string(),
                    root: store.root.clone(),
                })),
            }
        }
        output
    }

    fn discover_root(&mut self, root: &Path) {
        if self.stores.iter().any(|store| store.root == root) {
            return;
        }
        let metadata_path = root.join(".beads/metadata.json");
        if !metadata_path.is_file() {
            return;
        }
        match open_store(root, &metadata_path) {
            Ok(store) => {
                self.established.push(IntegrityEvent {
                    level: IntegrityLevel::Info,
                    source: "beads",
                    summary: "work history connected".into(),
                    detail: format!("watching {}", store.data_dir.display()),
                    root: root.to_path_buf(),
                });
                self.stores.push(store);
            }
            Err(error) => self.established.push(IntegrityEvent {
                level: IntegrityLevel::Degraded,
                source: "beads",
                summary: "work history unavailable".into(),
                detail: error.to_string(),
                root: root.to_path_buf(),
            }),
        }
    }
}

impl BeadsStore {
    fn observes_path(&self, path: &Path) -> bool {
        path == self.root || path.starts_with(self.root.join(".beads"))
    }
}

fn open_store(root: &Path, metadata_path: &Path) -> Result<BeadsStore> {
    let metadata: BeadsMetadata = serde_json::from_slice(
        &fs::read(metadata_path)
            .with_context(|| format!("could not read {}", metadata_path.display()))?,
    )
    .with_context(|| format!("could not parse {}", metadata_path.display()))?;
    if metadata
        .dolt_mode
        .as_deref()
        .is_some_and(|mode| mode != "embedded")
    {
        bail!(
            "unsupported Beads Dolt mode {}",
            metadata.dolt_mode.unwrap_or_default()
        );
    }
    let database = metadata
        .dolt_database
        .or(metadata.database)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Beads metadata does not name a Dolt database"))?;
    if !matches!(
        Path::new(&database)
            .components()
            .collect::<Vec<_>>()
            .as_slice(),
        [Component::Normal(_)]
    ) {
        bail!("Beads metadata contains an unsafe Dolt database name");
    }
    let data_dir = root.join(".beads/embeddeddolt").join(database);
    if !data_dir.is_dir() {
        bail!("Beads Dolt store is missing at {}", data_dir.display());
    }
    probe_schema(&data_dir)?;
    let cursor = current_head(&data_dir)?;
    Ok(BeadsStore {
        root: root.to_path_buf(),
        data_dir,
        cursor,
        dirty_since: None,
    })
}

fn probe_schema(data_dir: &Path) -> Result<()> {
    for table in REQUIRED_TABLES {
        run_json(
            data_dir,
            &format!("SELECT * FROM dolt_diff_{table} LIMIT 0"),
        )
        .with_context(|| format!("unsupported Beads schema: missing dolt_diff_{table}"))?;
    }
    Ok(())
}

fn advance_store(store: &mut BeadsStore) -> Result<Vec<BeadsActivity>> {
    if !store.data_dir.is_dir() {
        bail!("Beads Dolt store was removed or replaced");
    }
    let head = current_head(&store.data_dir)?;
    if head == store.cursor {
        return Ok(Vec::new());
    }
    let commits = commits_after(&store.data_dir, &store.cursor, &head)?;
    if commits.is_empty() {
        bail!("Beads history no longer contains cursor {}", store.cursor);
    }

    let mut activities = Vec::new();
    let mut previous = store.cursor.clone();
    for commit in commits {
        activities.extend(activities_between(
            &store.root,
            &store.data_dir,
            &previous,
            &commit,
        )?);
        previous = commit;
    }
    store.cursor = head;
    Ok(activities)
}

fn current_head(data_dir: &Path) -> Result<String> {
    let rows = run_json(
        data_dir,
        "SELECT commit_hash FROM dolt_log ORDER BY date DESC LIMIT 1",
    )?;
    rows.first()
        .and_then(|row| string(row, "commit_hash"))
        .ok_or_else(|| anyhow!("Beads Dolt history has no HEAD commit"))
}

fn commits_after(data_dir: &Path, cursor: &str, head: &str) -> Result<Vec<String>> {
    let output = Command::new("dolt")
        .arg("--data-dir")
        .arg(data_dir)
        .args(["log", "--oneline", &format!("{cursor}..{head}")])
        .output()
        .context("could not execute dolt; install Dolt to observe Beads work")?;
    if !output.status.success() {
        bail!(
            "could not traverse Beads history from {cursor}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut commits = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    commits.reverse();
    Ok(commits)
}

fn activities_between(
    root: &Path,
    data_dir: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<BeadsActivity>> {
    let mut activities = event_activities(root, data_dir, from, to)?;
    activities.extend(comment_activities(root, data_dir, from, to)?);
    activities.extend(dependency_activities(root, data_dir, from, to)?);
    activities.extend(label_activities(root, data_dir, from, to)?);
    if activities.is_empty() {
        activities.extend(issue_fallback_activities(root, data_dir, from, to)?);
    }
    Ok(activities)
}

fn event_activities(
    root: &Path,
    data_dir: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<BeadsActivity>> {
    let query = format!(
        "SELECT d.to_issue_id AS issue_id, COALESCE(i.title, '') AS title, \
         d.to_event_type AS action, d.to_actor AS actor, d.to_new_value AS new_value, \
         d.to_comment AS comment FROM dolt_diff_events d \
         LEFT JOIN issues i ON i.id = d.to_issue_id \
         WHERE d.from_commit='{from}' AND d.to_commit='{to}' AND d.diff_type='added' \
         ORDER BY d.to_created_at, d.to_id"
    );
    Ok(run_json(data_dir, &query)?
        .into_iter()
        .filter_map(|row| {
            let issue_id = string(&row, "issue_id")?;
            let action = string(&row, "action").unwrap_or_else(|| "updated".into());
            let actor = string(&row, "actor").unwrap_or_default();
            let new_value = string(&row, "new_value").unwrap_or_default();
            let comment = string(&row, "comment").unwrap_or_default();
            let detail = event_detail(&action, &actor, &new_value, &comment);
            Some(BeadsActivity {
                root: root.to_path_buf(),
                issue_id,
                title: string(&row, "title").unwrap_or_default(),
                action,
                detail,
            })
        })
        .collect())
}

fn comment_activities(
    root: &Path,
    data_dir: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<BeadsActivity>> {
    let query = format!(
        "SELECT d.to_issue_id AS issue_id, COALESCE(i.title, '') AS title, \
         d.to_author AS actor, d.to_text AS body FROM dolt_diff_comments d \
         LEFT JOIN issues i ON i.id = d.to_issue_id \
         WHERE d.from_commit='{from}' AND d.to_commit='{to}' AND d.diff_type='added' \
         ORDER BY d.to_created_at, d.to_id"
    );
    Ok(run_json(data_dir, &query)?
        .into_iter()
        .filter_map(|row| {
            let issue_id = string(&row, "issue_id")?;
            let actor = clean_actor(&string(&row, "actor").unwrap_or_default());
            let body = truncate(&string(&row, "body").unwrap_or_default(), 180);
            Some(BeadsActivity {
                root: root.to_path_buf(),
                issue_id,
                title: string(&row, "title").unwrap_or_default(),
                action: "commented".into(),
                detail: if actor.is_empty() {
                    body
                } else {
                    format!("{actor}: {body}")
                },
            })
        })
        .collect())
}

fn dependency_activities(
    root: &Path,
    data_dir: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<BeadsActivity>> {
    let query = format!(
        "SELECT COALESCE(d.to_issue_id, d.from_issue_id) AS issue_id, \
         COALESCE(i.title, '') AS title, d.diff_type, \
         COALESCE(d.to_type, d.from_type) AS dependency_type, \
         COALESCE(d.to_depends_on_issue_id, d.from_depends_on_issue_id, '') AS target \
         FROM dolt_diff_dependencies d \
         LEFT JOIN issues i ON i.id = COALESCE(d.to_issue_id, d.from_issue_id) \
         WHERE d.from_commit='{from}' AND d.to_commit='{to}'"
    );
    Ok(run_json(data_dir, &query)?
        .into_iter()
        .filter_map(|row| {
            let issue_id = string(&row, "issue_id")?;
            let added = string(&row, "diff_type").as_deref() == Some("added");
            let dependency_type =
                string(&row, "dependency_type").unwrap_or_else(|| "depends on".into());
            let target = string(&row, "target").unwrap_or_default();
            Some(BeadsActivity {
                root: root.to_path_buf(),
                issue_id,
                title: string(&row, "title").unwrap_or_default(),
                action: if added { "linked" } else { "unlinked" }.into(),
                detail: format!("{dependency_type} {target}"),
            })
        })
        .collect())
}

fn label_activities(
    root: &Path,
    data_dir: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<BeadsActivity>> {
    let query = format!(
        "SELECT COALESCE(d.to_issue_id, d.from_issue_id) AS issue_id, \
         COALESCE(i.title, '') AS title, d.diff_type, \
         COALESCE(d.to_label, d.from_label, '') AS label FROM dolt_diff_labels d \
         LEFT JOIN issues i ON i.id = COALESCE(d.to_issue_id, d.from_issue_id) \
         WHERE d.from_commit='{from}' AND d.to_commit='{to}'"
    );
    Ok(run_json(data_dir, &query)?
        .into_iter()
        .filter_map(|row| {
            let issue_id = string(&row, "issue_id")?;
            let added = string(&row, "diff_type").as_deref() == Some("added");
            let label = string(&row, "label").unwrap_or_default();
            Some(BeadsActivity {
                root: root.to_path_buf(),
                issue_id,
                title: string(&row, "title").unwrap_or_default(),
                action: if added { "labeled" } else { "unlabeled" }.into(),
                detail: label,
            })
        })
        .collect())
}

fn issue_fallback_activities(
    root: &Path,
    data_dir: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<BeadsActivity>> {
    let query = format!(
        "SELECT COALESCE(d.to_id, d.from_id) AS issue_id, \
         COALESCE(d.to_title, d.from_title, '') AS title, d.diff_type \
         FROM dolt_diff_issues d WHERE d.from_commit='{from}' AND d.to_commit='{to}'"
    );
    Ok(run_json(data_dir, &query)?
        .into_iter()
        .filter_map(|row| {
            let issue_id = string(&row, "issue_id")?;
            let diff_type = string(&row, "diff_type").unwrap_or_default();
            let action = match diff_type.as_str() {
                "added" => "created",
                "removed" => "deleted",
                _ => "updated",
            };
            Some(BeadsActivity {
                root: root.to_path_buf(),
                issue_id,
                title: string(&row, "title").unwrap_or_default(),
                action: action.into(),
                detail: "issue fields changed".into(),
            })
        })
        .collect())
}

fn event_detail(action: &str, actor: &str, new_value: &str, comment: &str) -> String {
    let actor = clean_actor(actor);
    let subject = match action {
        "closed" if !new_value.is_empty() => truncate(new_value, 180),
        "updated" => changed_fields(new_value),
        _ if !comment.is_empty() => truncate(comment, 180),
        _ => String::new(),
    };
    match (actor.is_empty(), subject.is_empty()) {
        (false, false) => format!("{actor} · {subject}"),
        (false, true) => format!("by {actor}"),
        (true, false) => subject,
        (true, true) => "authoritative Beads transition".into(),
    }
}

fn changed_fields(value: &str) -> String {
    let Ok(Value::Object(fields)) = serde_json::from_str(value) else {
        return truncate(value, 180);
    };
    let mut names = fields.keys().cloned().collect::<Vec<_>>();
    names.sort();
    if names.is_empty() {
        "issue fields changed".into()
    } else {
        format!("changed {}", names.join(", "))
    }
}

fn clean_actor(actor: &str) -> String {
    actor.trim().trim_matches('"').to_string()
}

fn truncate(value: &str, max_chars: usize) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let mut chars = sanitized.chars();
    let shortened = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

fn run_json(data_dir: &Path, query: &str) -> Result<Vec<Map<String, Value>>> {
    let output = Command::new("dolt")
        .arg("--data-dir")
        .arg(data_dir)
        .args(["sql", "-r", "json", "-q", query])
        .output()
        .context("could not execute dolt; install Dolt to observe Beads work")?;
    if !output.status.success() {
        bail!(
            "Dolt query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let parsed: JsonRows =
        serde_json::from_slice(&output.stdout).with_context(|| "Dolt returned malformed JSON")?;
    Ok(parsed.rows)
}

fn string(row: &Map<String, Value>, field: &str) -> Option<String> {
    match row.get(field)? {
        Value::String(value) => Some(value.clone()),
        Value::Null => None,
        value => Some(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::WatchState;
    use std::{process::Command, thread};

    #[test]
    fn summarizes_updated_fields_without_leaking_payloads() {
        assert_eq!(
            changed_fields(r#"{"title":"new","description":"body"}"#),
            "changed description, title"
        );
    }

    #[test]
    fn sanitizes_control_characters_in_work_text() {
        assert_eq!(
            truncate("line one\nline two\u{1b}", 80),
            "line one line two "
        );
    }

    #[test]
    fn coalesced_beads_parent_paths_trigger_the_store() {
        let store = BeadsStore {
            root: PathBuf::from("/project"),
            data_dir: PathBuf::from("/project/.beads/embeddeddolt/flux"),
            cursor: "head".into(),
            dirty_since: None,
        };
        assert!(store.observes_path(Path::new("/project/.beads")));
        assert!(store.observes_path(Path::new("/project/.beads/embeddeddolt")));
        assert!(store.observes_path(Path::new("/project/.beads/embeddeddolt/flux/.dolt/noms")));
        assert!(!store.observes_path(Path::new("/project/src/main.rs")));
    }

    #[test]
    fn flux_project_store_is_a_live_integration_fixture_when_present() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let metadata = root.join(".beads/metadata.json");
        if !metadata.is_file() || !root.join(".beads/embeddeddolt").is_dir() {
            return;
        }
        let store = open_store(&root, &metadata).expect("open Flux Beads store");
        let rows = run_json(
            &store.data_dir,
            "SELECT commit_hash FROM dolt_log ORDER BY date DESC LIMIT 32",
        )
        .expect("read Flux Beads history");
        let mut commits = rows
            .into_iter()
            .filter_map(|row| string(&row, "commit_hash"))
            .collect::<Vec<_>>();
        commits.reverse();
        let observed = commits.windows(2).any(|pair| {
            activities_between(&root, &store.data_dir, &pair[0], &pair[1]).is_ok_and(|activities| {
                activities
                    .iter()
                    .any(|activity| activity.issue_id.starts_with("flux-"))
            })
        });
        assert!(
            observed,
            "Flux's Beads history produced no semantic work event"
        );
    }

    #[test]
    #[ignore = "mutates the Flux project's live Beads history"]
    fn flux_project_observes_a_native_beads_transition_end_to_end() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .expect("canonical Flux root");
        if !root.join(".beads/metadata.json").is_file()
            || !root.join(".beads/embeddeddolt").is_dir()
        {
            return;
        }
        let watcher = WatchState::start(vec![root.clone()]).expect("start native watcher");
        let mut monitor = BeadsMonitor::discover(std::slice::from_ref(&root));
        assert_eq!(monitor.store_count(), 1);

        let marker = format!("Flux live adapter validation at {:?}", Instant::now());
        let status = Command::new("bd")
            .current_dir(&root)
            .args([
                "comment",
                "flux-4et.2",
                &marker,
                "--actor",
                "Flux integration test",
            ])
            .status()
            .expect("execute bd comment");
        assert!(status.success());

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match watcher.receiver.recv_timeout(Duration::from_millis(120)) {
                Ok(Ok(event)) => monitor.observe_workspace_event(&event),
                Ok(Err(error)) => panic!("native watcher failed: {error}"),
                Err(_) => break,
            }
        }
        thread::sleep(EVENT_QUIET_PERIOD + Duration::from_millis(50));
        let events = monitor.drain();
        assert!(events.iter().any(|event| matches!(
            event,
            BeadsMonitorEvent::Activity(activity)
                if activity.issue_id == "flux-4et.2"
                    && activity.action == "commented"
                    && activity.detail.contains(&marker)
        )));
    }
}
