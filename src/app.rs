use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use notify::{
    Event, EventKind,
    event::{ModifyKind, RenameMode},
};

use crate::{
    git::GitActivity,
    model::{ChangeEvent, ChangeKind, TargetKind},
    watcher::{Snapshot, WatchState, build_diff, ignored},
};

const RENAME_TIMEOUT: Duration = Duration::from_millis(400);
const DEDUP_WINDOW: Duration = Duration::from_millis(80);

pub struct App {
    pub events: VecDeque<ChangeEvent>,
    pub paused_events: VecDeque<ChangeEvent>,
    pub selected: usize,
    pub paused: bool,
    pub should_quit: bool,
    pub search: String,
    pub search_mode: bool,
    pub enabled: [bool; 5],
    pub status: Option<String>,
    pub max_events: usize,
    next_id: u64,
    pending_rename: Option<(PathBuf, Snapshot, Instant)>,
    recent: HashMap<PathBuf, (ChangeKind, Instant)>,
}

impl App {
    pub fn new(max_events: usize) -> Self {
        Self {
            events: VecDeque::new(),
            paused_events: VecDeque::new(),
            selected: 0,
            paused: false,
            should_quit: false,
            search: String::new(),
            search_mode: false,
            enabled: [true; 5],
            status: None,
            max_events,
            next_id: 1,
            pending_rename: None,
            recent: HashMap::new(),
        }
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        let query = self.search.to_lowercase();
        self.events
            .iter()
            .enumerate()
            .filter(|(_, event)| self.kind_enabled(event.kind))
            .filter(|(_, event)| {
                query.is_empty()
                    || event.path.to_string_lossy().to_lowercase().contains(&query)
                    || event
                        .previous_path
                        .as_ref()
                        .is_some_and(|path| path.to_string_lossy().to_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn selected_event(&self) -> Option<&ChangeEvent> {
        let visible = self.visible_indices();
        visible
            .get(self.selected)
            .and_then(|index| self.events.get(*index))
    }

    pub fn move_selection(&mut self, delta: isize) {
        let length = self.visible_indices().len();
        if length == 0 {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.saturating_add_signed(delta).min(length - 1);
    }

    pub fn toggle_kind(&mut self, index: usize) {
        if let Some(enabled) = self.enabled.get_mut(index) {
            *enabled = !*enabled;
            self.selected = 0;
        }
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if !self.paused {
            while let Some(event) = self.paused_events.pop_back() {
                self.push_event(event);
            }
        }
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.paused_events.clear();
        self.selected = 0;
    }

    pub fn process_notify(&mut self, watcher: &mut WatchState, event: Event) {
        self.status = None;
        let paths: Vec<_> = event
            .paths
            .into_iter()
            .filter(|path| !ignored(path))
            .collect();
        if paths.is_empty() {
            return;
        }

        match event.kind {
            EventKind::Create(_) => {
                for path in paths {
                    self.created(watcher, path);
                }
            }
            EventKind::Remove(_) => {
                for path in paths {
                    self.deleted(watcher, path);
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if paths.len() >= 2 => {
                self.renamed(watcher, &paths[0], &paths[1]);
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                let path = paths[0].clone();
                let snapshot = watcher.remove_snapshot(&path);
                self.pending_rename = Some((path, snapshot, Instant::now()));
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                if let Some((from, previous, _)) = self.pending_rename.take() {
                    self.finish_rename(watcher, &from, &paths[0], previous);
                } else {
                    self.created(watcher, paths[0].clone());
                }
            }
            EventKind::Modify(ModifyKind::Name(_)) if paths.len() >= 2 => {
                self.renamed(watcher, &paths[0], &paths[1]);
            }
            EventKind::Modify(_) => {
                for path in paths {
                    self.modified(watcher, path);
                }
            }
            EventKind::Access(_) | EventKind::Other | EventKind::Any => {}
        }
    }

    pub fn process_git(&mut self, activity: GitActivity) {
        let event = ChangeEvent {
            id: self.next_id,
            kind: ChangeKind::Git,
            target: TargetKind::Repository,
            path: PathBuf::from(activity.summary),
            previous_path: None,
            root: activity.root,
            occurred_at: Instant::now(),
            size: 0,
            lines_added: 0,
            lines_removed: 0,
            diff: Vec::new(),
            detail: Some(activity.detail),
        };
        self.next_id += 1;
        if self.paused {
            self.paused_events.push_front(event);
        } else {
            self.push_event(event);
        }
    }

    pub fn flush_pending(&mut self, watcher: &WatchState) {
        let expired = self
            .pending_rename
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() >= RENAME_TIMEOUT);
        if expired {
            let (path, snapshot, _) = self.pending_rename.take().expect("pending rename exists");
            self.emit_basic(watcher, ChangeKind::Delete, path, snapshot, None);
        }
        self.recent
            .retain(|_, (_, seen)| seen.elapsed() < Duration::from_secs(2));
    }

    fn created(&mut self, watcher: &mut WatchState, path: PathBuf) {
        let snapshot = watcher.take_snapshot(&path);
        let diff = build_diff(Some(""), snapshot.content.as_deref());
        self.emit(
            watcher,
            ChangeKind::Create,
            path,
            snapshot,
            None,
            diff.lines,
            diff.added,
            diff.removed,
            diff.detail,
        );
    }

    fn modified(&mut self, watcher: &mut WatchState, path: PathBuf) {
        if !path.exists() {
            return;
        }
        let previous = watcher.previous_snapshot(&path).cloned();
        let current = watcher.take_snapshot(&path);
        if current.is_dir {
            return;
        }
        let diff = build_diff(
            previous
                .as_ref()
                .and_then(|snapshot| snapshot.content.as_deref()),
            current.content.as_deref(),
        );
        if diff.added == 0
            && diff.removed == 0
            && previous
                .as_ref()
                .is_some_and(|old| old.size == current.size)
        {
            return;
        }
        self.emit(
            watcher,
            ChangeKind::Modify,
            path,
            current,
            None,
            diff.lines,
            diff.added,
            diff.removed,
            diff.detail,
        );
    }

    fn deleted(&mut self, watcher: &mut WatchState, path: PathBuf) {
        let snapshot = watcher.remove_snapshot(&path);
        self.emit_basic(watcher, ChangeKind::Delete, path, snapshot, None);
    }

    fn renamed(&mut self, watcher: &mut WatchState, from: &Path, to: &Path) {
        let previous = watcher.remove_snapshot(from);
        self.finish_rename(watcher, from, to, previous);
    }

    fn finish_rename(
        &mut self,
        watcher: &mut WatchState,
        from: &Path,
        to: &Path,
        previous: Snapshot,
    ) {
        let current = watcher.take_snapshot(to);
        let diff = build_diff(previous.content.as_deref(), current.content.as_deref());
        self.emit(
            watcher,
            ChangeKind::Rename,
            to.to_path_buf(),
            current,
            Some(watcher.relative_path(from)),
            diff.lines,
            diff.added,
            diff.removed,
            diff.detail,
        );
    }

    fn emit_basic(
        &mut self,
        watcher: &WatchState,
        kind: ChangeKind,
        path: PathBuf,
        snapshot: Snapshot,
        previous_path: Option<PathBuf>,
    ) {
        self.emit(
            watcher,
            kind,
            path,
            snapshot,
            previous_path,
            Vec::new(),
            0,
            0,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        watcher: &WatchState,
        kind: ChangeKind,
        absolute_path: PathBuf,
        snapshot: Snapshot,
        previous_path: Option<PathBuf>,
        diff: Vec<crate::model::DiffLine>,
        lines_added: usize,
        lines_removed: usize,
        detail: Option<String>,
    ) {
        let path = watcher.relative_path(&absolute_path);
        let root = watcher.root_for(&absolute_path);
        let duplicate = self
            .recent
            .get(&absolute_path)
            .is_some_and(|(previous, seen)| {
                seen.elapsed() < DEDUP_WINDOW
                    && (*previous == kind
                        || matches!(
                            (*previous, kind),
                            (ChangeKind::Create, ChangeKind::Modify)
                                | (ChangeKind::Rename, ChangeKind::Modify)
                        ))
            });
        if duplicate {
            return;
        }
        self.recent.insert(absolute_path, (kind, Instant::now()));

        let event = ChangeEvent {
            id: self.next_id,
            kind,
            target: if snapshot.is_dir {
                TargetKind::Directory
            } else {
                TargetKind::File
            },
            path,
            previous_path,
            root,
            occurred_at: Instant::now(),
            size: snapshot.size,
            lines_added,
            lines_removed,
            diff,
            detail,
        };
        self.next_id += 1;

        if self.paused {
            self.paused_events.push_front(event);
        } else {
            self.push_event(event);
        }
    }

    fn push_event(&mut self, event: ChangeEvent) {
        if self.selected > 0 {
            self.selected += 1;
        }
        self.events.push_front(event);
        while self.events.len() > self.max_events {
            self.events.pop_back();
        }
    }

    fn kind_enabled(&self, kind: ChangeKind) -> bool {
        let index = ChangeKind::ALL
            .iter()
            .position(|candidate| *candidate == kind)
            .unwrap_or(0);
        self.enabled[index]
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::SystemTime};

    use notify::{
        Event, EventKind,
        event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode},
    };

    use super::*;

    #[test]
    fn translates_create_modify_rename_and_delete() {
        let root = temporary_directory("pipeline");
        let first = root.join("first.txt");
        let second = root.join("second.txt");
        let mut watcher = WatchState::start(vec![root.clone()]).expect("watch state");
        let mut app = App::new(20);

        fs::write(&first, "one\n").expect("create file");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Create(CreateKind::File)).add_path(first.clone()),
        );

        thread::sleep(DEDUP_WINDOW + Duration::from_millis(10));
        fs::write(&first, "one\ntwo\n").expect("modify file");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                .add_path(first.clone()),
        );

        fs::rename(&first, &second).expect("rename file");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path(first)
                .add_path(second.clone()),
        );

        thread::sleep(DEDUP_WINDOW + Duration::from_millis(10));
        fs::remove_file(&second).expect("delete file");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Remove(RemoveKind::File)).add_path(second),
        );

        let kinds = app
            .events
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                ChangeKind::Delete,
                ChangeKind::Rename,
                ChangeKind::Modify,
                ChangeKind::Create,
            ]
        );
        let modified = app
            .events
            .iter()
            .find(|event| event.kind == ChangeKind::Modify)
            .expect("modify event");
        assert_eq!((modified.lines_added, modified.lines_removed), (1, 0));
        let _ = fs::remove_dir_all(root);
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
