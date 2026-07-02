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
    model::{ChangeEvent, ChangeKind, IntegrityEvent, IntegrityLevel, TargetKind},
    watcher::{Snapshot, WatchState, build_diff, ignored},
};

const RENAME_TIMEOUT: Duration = Duration::from_millis(400);
const DEDUP_WINDOW: Duration = Duration::from_millis(80);

struct PendingRename {
    path: PathBuf,
    snapshot: Snapshot,
    tracker: Option<usize>,
    observed_at: Instant,
}

pub struct App {
    pub events: VecDeque<ChangeEvent>,
    pub paused_events: VecDeque<ChangeEvent>,
    pub selected: usize,
    pub paused: bool,
    pub should_quit: bool,
    pub search: String,
    pub search_mode: bool,
    pub enabled: [bool; 6],
    pub status: Option<String>,
    pub catching_up: bool,
    pub workspace_filter: Option<PathBuf>,
    pub max_events: usize,
    observer_level: IntegrityLevel,
    next_id: u64,
    pending_renames: Vec<PendingRename>,
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
            enabled: [true; 6],
            status: None,
            catching_up: false,
            workspace_filter: None,
            max_events,
            observer_level: IntegrityLevel::Info,
            next_id: 1,
            pending_renames: Vec::new(),
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
                self.workspace_filter
                    .as_ref()
                    .is_none_or(|root| &event.root == root)
            })
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

    pub fn observer_level(&self) -> IntegrityLevel {
        self.observer_level
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

    pub fn cycle_workspace(&mut self, roots: &[PathBuf], reverse: bool) {
        if roots.is_empty() {
            self.workspace_filter = None;
            return;
        }
        let next = match (&self.workspace_filter, reverse) {
            (None, false) => Some(roots[0].clone()),
            (None, true) => roots.last().cloned(),
            (Some(current), false) => roots
                .iter()
                .position(|root| root == current)
                .and_then(|index| roots.get(index + 1).cloned()),
            (Some(current), true) => roots
                .iter()
                .position(|root| root == current)
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| roots.get(index).cloned()),
        };
        self.workspace_filter = next;
        self.selected = 0;
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
        if event.need_rescan() {
            let root = event
                .paths
                .first()
                .map(|path| watcher.root_for(path))
                .or_else(|| watcher.roots.first().cloned())
                .unwrap_or_default();
            self.process_integrity(IntegrityEvent {
                level: IntegrityLevel::Uncertain,
                source: "filesystem",
                summary: "native events may have been lost".into(),
                detail: event
                    .info()
                    .unwrap_or("the operating system requested a reconciliation scan")
                    .into(),
                root,
            });
            return;
        }
        let tracker = event.attrs.tracker();
        if matches!(event.kind, EventKind::Remove(_)) {
            for path in &event.paths {
                if watcher.is_root(path) {
                    self.process_integrity(IntegrityEvent {
                        level: IntegrityLevel::Lost,
                        source: "filesystem",
                        summary: "watched root removed".into(),
                        detail: "native observation cannot continue for this root".into(),
                        root: path.clone(),
                    });
                }
            }
        }
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
                self.pending_renames.push(PendingRename {
                    path,
                    snapshot,
                    tracker,
                    observed_at: Instant::now(),
                });
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                let matching = tracker
                    .and_then(|tracker| {
                        self.pending_renames
                            .iter()
                            .position(|pending| pending.tracker == Some(tracker))
                    })
                    .or_else(|| (!self.pending_renames.is_empty()).then_some(0));
                if let Some(index) = matching {
                    let pending = self.pending_renames.remove(index);
                    self.finish_rename(watcher, &pending.path, &paths[0], pending.snapshot);
                } else {
                    self.created(watcher, paths[0].clone());
                }
            }
            EventKind::Modify(ModifyKind::Name(_)) if paths.len() >= 2 => {
                self.renamed(watcher, &paths[0], &paths[1]);
            }
            EventKind::Modify(ModifyKind::Name(_)) => {
                let path = paths[0].clone();
                if !path.exists() && watcher.previous_snapshot(&path).is_some() {
                    let snapshot = watcher.remove_snapshot(&path);
                    self.pending_renames.push(PendingRename {
                        path,
                        snapshot,
                        tracker,
                        observed_at: Instant::now(),
                    });
                } else if path.exists() {
                    let matching = tracker
                        .and_then(|tracker| {
                            self.pending_renames
                                .iter()
                                .position(|pending| pending.tracker == Some(tracker))
                        })
                        .or_else(|| (!self.pending_renames.is_empty()).then_some(0));
                    if let Some(index) = matching {
                        let pending = self.pending_renames.remove(index);
                        self.finish_rename(watcher, &pending.path, &path, pending.snapshot);
                    } else if watcher.previous_snapshot(&path).is_some() {
                        self.modified(watcher, path);
                    } else {
                        self.created(watcher, path);
                    }
                }
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
            integrity_level: None,
        };
        self.next_id += 1;
        if self.paused {
            self.paused_events.push_front(event);
        } else {
            self.push_event(event);
        }
    }

    pub fn process_integrity(&mut self, integrity: IntegrityEvent) {
        if integrity.level.severity() > self.observer_level.severity() {
            self.observer_level = integrity.level;
        }
        let event = ChangeEvent {
            id: self.next_id,
            kind: ChangeKind::Integrity,
            target: TargetKind::Observer,
            path: PathBuf::from(format!("{} · {}", integrity.source, integrity.summary)),
            previous_path: None,
            root: integrity.root,
            occurred_at: Instant::now(),
            size: 0,
            lines_added: 0,
            lines_removed: 0,
            diff: Vec::new(),
            detail: Some(integrity.detail),
            integrity_level: Some(integrity.level),
        };
        self.next_id += 1;
        if self.paused {
            self.paused_events.push_front(event);
        } else {
            self.push_event(event);
        }
    }

    pub fn flush_pending(&mut self, watcher: &WatchState) {
        let mut index = 0;
        while index < self.pending_renames.len() {
            if self.pending_renames[index].observed_at.elapsed() >= RENAME_TIMEOUT {
                let pending = self.pending_renames.remove(index);
                self.emit_basic(
                    watcher,
                    ChangeKind::Delete,
                    pending.path,
                    pending.snapshot,
                    None,
                );
            } else {
                index += 1;
            }
        }
        self.recent
            .retain(|_, (_, seen)| seen.elapsed() < Duration::from_secs(2));
    }

    fn created(&mut self, watcher: &mut WatchState, path: PathBuf) {
        if watcher.previous_snapshot(&path).is_some() {
            self.modified(watcher, path);
            return;
        }
        let snapshot = watcher.take_snapshot(&path);
        self.report_snapshot_issue(watcher, &path, &snapshot);
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
        self.report_snapshot_issue(watcher, &path, &current);
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
            if let Some(existing) = self.events.iter_mut().find(|event| {
                event.root.join(&event.path) == absolute_path
                    && (event.kind == kind
                        || matches!(
                            (event.kind, kind),
                            (ChangeKind::Create, ChangeKind::Modify)
                                | (ChangeKind::Rename, ChangeKind::Modify)
                        ))
            }) {
                existing.occurred_at = Instant::now();
                existing.size = snapshot.size;
                existing.lines_added = lines_added;
                existing.lines_removed = lines_removed;
                existing.diff = diff;
                existing.detail = detail;
                return;
            }
            if let Some(existing) = self.paused_events.iter_mut().find(|event| {
                event.root.join(&event.path) == absolute_path
                    && (event.kind == kind
                        || matches!(
                            (event.kind, kind),
                            (ChangeKind::Create, ChangeKind::Modify)
                                | (ChangeKind::Rename, ChangeKind::Modify)
                        ))
            }) {
                existing.occurred_at = Instant::now();
                existing.size = snapshot.size;
                existing.lines_added = lines_added;
                existing.lines_removed = lines_removed;
                existing.diff = diff;
                existing.detail = detail;
            }
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
            integrity_level: None,
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

    fn report_snapshot_issue(&mut self, watcher: &WatchState, path: &Path, snapshot: &Snapshot) {
        let Some(detail) = snapshot.read_error.clone() else {
            return;
        };
        if !path.exists() {
            return;
        }
        self.process_integrity(IntegrityEvent {
            level: IntegrityLevel::Degraded,
            source: "filesystem",
            summary: "changed path could not be inspected".into(),
            detail,
            root: watcher.root_for(path),
        });
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
        event::Flag,
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

    #[test]
    fn coalesced_modify_updates_the_visible_create_event() {
        let root = temporary_directory("coalesce");
        let path = root.join("burst.txt");
        let mut watcher = WatchState::start(vec![root.clone()]).expect("watch state");
        let mut app = App::new(20);

        fs::write(&path, "one\n").expect("create file");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Create(CreateKind::File)).add_path(path.clone()),
        );
        fs::write(&path, "one\ntwo\n").expect("modify file");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content))).add_path(path),
        );

        let file_events = app
            .events
            .iter()
            .filter(|event| event.target == TargetKind::File)
            .collect::<Vec<_>>();
        assert_eq!(file_events.len(), 1);
        assert_eq!(file_events[0].kind, ChangeKind::Create);
        assert_eq!(file_events[0].size, 8);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn split_renames_are_paired_by_native_tracker() {
        let root = temporary_directory("rename-tracker");
        let first = root.join("first.txt");
        let second = root.join("second.txt");
        let first_moved = root.join("first-moved.txt");
        let second_moved = root.join("second-moved.txt");
        fs::write(&first, "first\n").expect("first file");
        fs::write(&second, "second\n").expect("second file");
        let mut watcher = WatchState::start(vec![root.clone()]).expect("watch state");
        let mut app = App::new(20);

        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                .add_path(first.clone())
                .set_tracker(10),
        );
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                .add_path(second.clone())
                .set_tracker(20),
        );
        fs::rename(&second, &second_moved).expect("move second");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
                .add_path(second_moved.clone())
                .set_tracker(20),
        );
        fs::rename(&first, &first_moved).expect("move first");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
                .add_path(first_moved.clone())
                .set_tracker(10),
        );

        let pairs = app
            .events
            .iter()
            .filter(|event| event.kind == ChangeKind::Rename)
            .map(|event| {
                (
                    event.previous_path.clone().expect("previous path"),
                    event.path.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert!(pairs.contains(&(PathBuf::from("first.txt"), PathBuf::from("first-moved.txt"))));
        assert!(pairs.contains(&(
            PathBuf::from("second.txt"),
            PathBuf::from("second-moved.txt")
        )));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn one_path_rename_events_are_paired_from_snapshot_evidence() {
        let root = temporary_directory("imprecise-rename");
        let from = root.join("before.txt");
        let to = root.join("after.txt");
        fs::write(&from, "content\n").expect("file");
        let mut watcher = WatchState::start(vec![root.clone()]).expect("watch state");
        let mut app = App::new(20);

        fs::rename(&from, &to).expect("rename");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Any))).add_path(from.clone()),
        );
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Any))).add_path(to.clone()),
        );

        let event = app
            .events
            .iter()
            .find(|event| event.kind == ChangeKind::Rename)
            .expect("rename event");
        assert_eq!(event.previous_path, Some(PathBuf::from("before.txt")));
        assert_eq!(event.path, PathBuf::from("after.txt"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn create_signal_for_existing_path_is_classified_as_modify() {
        let root = temporary_directory("replacement-create");
        let path = root.join("existing.txt");
        fs::write(&path, "before\n").expect("file");
        let mut watcher = WatchState::start(vec![root.clone()]).expect("watch state");
        let mut app = App::new(20);
        fs::write(&path, "after\n").expect("replace");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Create(CreateKind::File)).add_path(path),
        );
        assert_eq!(
            app.events.front().map(|event| event.kind),
            Some(ChangeKind::Modify)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn watched_root_removal_emits_lost_integrity() {
        let root = temporary_directory("root-loss");
        let mut watcher = WatchState::start(vec![root.clone()]).expect("watch state");
        let mut app = App::new(20);
        fs::remove_dir_all(&root).expect("remove root");
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(root),
        );
        assert!(app.events.iter().any(|event| {
            event.kind == ChangeKind::Integrity
                && event.integrity_level == Some(IntegrityLevel::Lost)
        }));
    }

    #[test]
    fn rescan_sentinel_emits_uncertain_integrity() {
        let root = temporary_directory("rescan");
        let mut watcher = WatchState::start(vec![root.clone()]).expect("watch state");
        let mut app = App::new(20);
        app.process_notify(
            &mut watcher,
            Event::new(EventKind::Other)
                .set_flag(Flag::Rescan)
                .set_info("rescan: kernel dropped"),
        );
        assert!(app.events.iter().any(|event| {
            event.kind == ChangeKind::Integrity
                && event.integrity_level == Some(IntegrityLevel::Uncertain)
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clearing_timeline_does_not_reset_observer_health() {
        let mut app = App::new(20);
        app.process_integrity(IntegrityEvent {
            level: IntegrityLevel::Lost,
            source: "filesystem",
            summary: "watch lost".into(),
            detail: "test".into(),
            root: PathBuf::from("/workspace"),
        });
        app.clear();
        assert_eq!(app.observer_level(), IntegrityLevel::Lost);
        assert!(app.events.is_empty());
    }

    #[test]
    fn workspace_focus_cycles_and_filters_without_reordering() {
        let first = PathBuf::from("/workspace/first");
        let second = PathBuf::from("/workspace/second");
        let roots = vec![first.clone(), second.clone()];
        let mut app = App::new(20);
        for root in &roots {
            app.process_integrity(IntegrityEvent {
                level: IntegrityLevel::Info,
                source: "filesystem",
                summary: "watch established".into(),
                detail: "native recursive filesystem events are active".into(),
                root: root.clone(),
            });
        }

        assert_eq!(app.visible_indices().len(), 2);
        app.cycle_workspace(&roots, false);
        assert_eq!(app.workspace_filter, Some(first));
        assert_eq!(app.visible_indices().len(), 1);
        app.cycle_workspace(&roots, false);
        assert_eq!(app.workspace_filter, Some(second));
        assert_eq!(app.visible_indices().len(), 1);
        app.cycle_workspace(&roots, false);
        assert_eq!(app.workspace_filter, None);
        assert_eq!(app.visible_indices().len(), 2);
        app.cycle_workspace(&roots, true);
        assert_eq!(app.workspace_filter, roots.last().cloned());
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
