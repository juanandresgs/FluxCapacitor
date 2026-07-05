mod app;
mod beads;
mod git;
mod model;
mod ui;
mod watcher;

use std::{
    io::{self, stdout},
    path::PathBuf,
    sync::mpsc::TryRecvError,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use app::App;
use beads::{BeadsMonitor, BeadsMonitorEvent};
use clap::Parser;
use crossterm::{
    event::{self, Event as TerminalEvent, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use git::{GitMonitor, GitMonitorEvent};
use model::{IntegrityEvent, IntegrityLevel};
use ratatui::{Terminal, backend::CrosstermBackend};
use watcher::WatchState;

const MAX_FILESYSTEM_EVENTS_PER_TICK: usize = 512;
const MAX_INTERNAL_EVENTS_PER_TICK: usize = 4096;
const MAX_BASELINE_ENTRIES_PER_TICK: usize = 1024;

#[derive(Parser, Debug)]
#[command(
    name = "flux",
    version,
    about = "Watch filesystem changes in a live terminal timeline"
)]
struct Cli {
    /// Directories to watch. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Maximum number of events retained in memory.
    #[arg(long, default_value_t = 1000)]
    max_events: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = expand_related_paths(resolve_paths(cli.paths)?);
    let mut watcher = WatchState::start(paths)?;
    let mut git_monitor = GitMonitor::discover(&watcher.roots)?;
    let mut beads_monitor = BeadsMonitor::discover(&watcher.roots)?;
    let mut app = App::new(cli.max_events.max(1));
    for event in watcher.established_events() {
        app.process_integrity(event);
    }
    for event in git_monitor.established_events() {
        app.process_integrity(event);
    }
    for event in beads_monitor.established_events() {
        app.process_integrity(event);
    }

    install_panic_hook();
    enable_raw_mode().context("failed to enter raw terminal mode")?;
    execute!(stdout(), EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("failed to initialize terminal")?;

    let result = run(
        &mut terminal,
        &mut app,
        &mut watcher,
        &mut git_monitor,
        &mut beads_monitor,
    );
    restore_terminal(&mut terminal)?;
    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    watcher: &mut WatchState,
    git_monitor: &mut GitMonitor,
    beads_monitor: &mut BeadsMonitor,
) -> Result<()> {
    let mut filesystem_disconnected = false;
    while !app.should_quit {
        let mut filesystem_events = 0;
        let mut internal_events = 0;
        loop {
            if filesystem_events >= MAX_FILESYSTEM_EVENTS_PER_TICK
                || internal_events >= MAX_INTERNAL_EVENTS_PER_TICK
            {
                app.catching_up = true;
                break;
            }
            match watcher.receiver.try_recv() {
                Ok(Ok(event)) => {
                    internal_events += 1;
                    let visible = visible_filesystem_event(&event);
                    if visible {
                        filesystem_events += 1;
                    }
                    beads_monitor.observe_workspace_event(&event);
                    for git_event in git_monitor.observe_workspace_event(&event) {
                        process_git_monitor_event(
                            app,
                            watcher,
                            git_monitor,
                            beads_monitor,
                            git_event,
                        );
                    }
                    if visible {
                        app.process_notify(watcher, event);
                    }
                }
                Ok(Err(error)) => {
                    let fallback = watcher.roots.first().cloned().unwrap_or_default();
                    app.process_integrity(watcher::integrity_from_notify_error(
                        "filesystem",
                        error,
                        &fallback,
                    ));
                }
                Err(TryRecvError::Empty) => {
                    app.catching_up = false;
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    if !filesystem_disconnected {
                        filesystem_disconnected = true;
                        app.process_integrity(IntegrityEvent {
                            level: IntegrityLevel::Lost,
                            source: "filesystem",
                            summary: "event channel disconnected".into(),
                            detail: "filesystem changes are no longer observable".into(),
                            root: watcher.roots.first().cloned().unwrap_or_default(),
                        });
                    }
                    break;
                }
            }
        }
        watcher.seed_step(MAX_BASELINE_ENTRIES_PER_TICK);
        for event in git_monitor.drain() {
            process_git_monitor_event(app, watcher, git_monitor, beads_monitor, event);
        }
        for event in beads_monitor.drain() {
            process_beads_monitor_event(app, event);
        }
        app.flush_pending(watcher);
        terminal.draw(|frame| {
            ui::draw(
                frame,
                app,
                watcher,
                git_monitor.repository_count(),
                beads_monitor.store_count(),
                &beads_monitor.diagnostics(),
            )
        })?;

        if event::poll(Duration::from_millis(80))?
            && let TerminalEvent::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(app, key.code, key.modifiers, &watcher.roots);
        }
    }
    Ok(())
}

fn visible_filesystem_event(event: &notify::Event) -> bool {
    event.need_rescan() || event.paths.iter().any(|path| !watcher::ignored(path))
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers, roots: &[PathBuf]) {
    if app.search_mode {
        match code {
            KeyCode::Esc | KeyCode::Enter => app.search_mode = false,
            KeyCode::Backspace => {
                app.search.pop();
                app.selected = 0;
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                app.search.push(character);
                app.selected = 0;
            }
            _ => {}
        }
        return;
    }

    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
        KeyCode::Home | KeyCode::Char('g') => app.selected = 0,
        KeyCode::End | KeyCode::Char('G') => {
            app.selected = app.visible_indices().len().saturating_sub(1)
        }
        KeyCode::Char('/') => app.search_mode = true,
        KeyCode::Esc if !app.search.is_empty() => {
            app.search.clear();
            app.selected = 0;
        }
        KeyCode::Char(' ') => app.toggle_pause(),
        KeyCode::Char('c') => app.clear(),
        KeyCode::Char('1') => app.toggle_kind(0),
        KeyCode::Char('2') => app.toggle_kind(1),
        KeyCode::Char('3') => app.toggle_kind(2),
        KeyCode::Char('4') => app.toggle_kind(3),
        KeyCode::Char('5') => app.toggle_kind(4),
        KeyCode::Char('6') => app.toggle_kind(5),
        KeyCode::Char('7') => app.toggle_kind(6),
        KeyCode::Char('w') => app.cycle_workspace(roots, false),
        KeyCode::Char('W') => app.cycle_workspace(roots, true),
        KeyCode::Char('t') => app.folders_open = !app.folders_open,
        _ => {}
    }
}

fn process_git_monitor_event(
    app: &mut App,
    watcher: &mut WatchState,
    git_monitor: &mut GitMonitor,
    beads_monitor: &mut BeadsMonitor,
    event: GitMonitorEvent,
) {
    match event {
        GitMonitorEvent::Activity(activity) => app.process_git(activity),
        GitMonitorEvent::Integrity(integrity) => app.process_integrity(integrity),
        GitMonitorEvent::RelatedWorktree(root) => match watcher.add_root(root.clone()) {
            Ok(Some(integrity)) => {
                app.process_integrity(integrity);
                for event in beads_monitor.add_root(&root) {
                    app.process_integrity(event);
                }
                for event in git_monitor.add_root(&root) {
                    match event {
                        GitMonitorEvent::Activity(activity) => app.process_git(activity),
                        GitMonitorEvent::Integrity(integrity) => app.process_integrity(integrity),
                        GitMonitorEvent::RelatedWorktree(_) => {}
                    }
                }
            }
            Ok(None) => {}
            Err(error) => app.process_integrity(IntegrityEvent {
                level: IntegrityLevel::Lost,
                source: "filesystem",
                summary: "related folder could not join".into(),
                detail: error.to_string(),
                root,
            }),
        },
    }
}

fn process_beads_monitor_event(app: &mut App, event: BeadsMonitorEvent) {
    match event {
        BeadsMonitorEvent::Activity(activity) => app.process_beads(activity),
        BeadsMonitorEvent::Integrity(integrity) => app.process_integrity(integrity),
    }
}

fn expand_related_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    for path in git::related_worktrees(&paths) {
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

fn resolve_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let paths = if paths.is_empty() {
        vec![std::env::current_dir().context("failed to read current directory")?]
    } else {
        paths
    };
    let mut resolved = Vec::new();
    for path in paths {
        let path = path
            .canonicalize()
            .with_context(|| format!("cannot access {}", path.display()))?;
        if !path.is_dir() {
            bail!("{} is not a directory", path.display());
        }
        if !resolved.contains(&path) {
            resolved.push(path);
        }
    }
    Ok(resolved)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode().context("failed to leave raw terminal mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    terminal.show_cursor().context("failed to restore cursor")?;
    Ok(())
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::EventKind;

    #[test]
    fn internal_metadata_events_do_not_consume_file_timeline_budget() {
        let beads = notify::Event::new(EventKind::Any)
            .add_path(PathBuf::from("project/.beads/last-touched"));
        let git = notify::Event::new(EventKind::Any).add_path(PathBuf::from("project/.git/index"));
        let source =
            notify::Event::new(EventKind::Any).add_path(PathBuf::from("project/src/main.rs"));
        assert!(!visible_filesystem_event(&beads));
        assert!(!visible_filesystem_event(&git));
        assert!(visible_filesystem_event(&source));
    }
}
