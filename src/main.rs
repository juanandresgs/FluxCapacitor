mod app;
mod git;
mod model;
mod ui;
mod watcher;

use std::{
    io::{self, stdout},
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::TryRecvError,
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use app::App;
use clap::Parser;
use crossterm::{
    cursor::Show,
    event::{self, Event as TerminalEvent, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use git::{GitMonitor, GitMonitorEvent};
use model::{IntegrityEvent, IntegrityLevel};
use ratatui::{Terminal, backend::CrosstermBackend};
use watcher::WatchState;

const MAX_FILESYSTEM_EVENTS_PER_TICK: usize = 512;
const MAX_BASELINE_ENTRIES_PER_TICK: usize = 1024;
static TERMINATION_REQUESTED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

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

    /// Maximum bytes retained for text diff baselines.
    #[arg(long, default_value_t = watcher::DEFAULT_SNAPSHOT_BYTES)]
    max_snapshot_bytes: usize,

    /// Maximum number of filesystem baselines retained in memory.
    #[arg(long, default_value_t = watcher::DEFAULT_SNAPSHOT_ENTRIES)]
    max_snapshots: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = expand_related_paths(resolve_paths(cli.paths)?);
    let mut watcher =
        WatchState::start_with_snapshot_limits(paths, cli.max_snapshot_bytes, cli.max_snapshots)?;
    let mut git_monitor = GitMonitor::discover(&watcher.roots)?;
    let mut app = App::new(cli.max_events.max(1));
    for event in watcher.established_events() {
        app.process_integrity(event);
    }
    for event in git_monitor.established_events() {
        app.process_integrity(event);
    }

    install_panic_hook();
    install_termination_handler()?;
    let mut terminal_guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("failed to initialize terminal")?;

    let result = run(&mut terminal, &mut app, &mut watcher, &mut git_monitor);
    drop(terminal);
    let restore_result = terminal_guard.restore();
    result?;
    restore_result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    watcher: &mut WatchState,
    git_monitor: &mut GitMonitor,
) -> Result<()> {
    let mut filesystem_disconnected = false;
    while !app.should_quit && !termination_requested().load(Ordering::Relaxed) {
        let mut filesystem_events = 0;
        loop {
            if filesystem_events >= MAX_FILESYSTEM_EVENTS_PER_TICK {
                app.catching_up = true;
                break;
            }
            match watcher.receiver.try_recv() {
                Ok(Ok(event)) => {
                    filesystem_events += 1;
                    for git_event in git_monitor.observe_workspace_event(&event) {
                        process_git_monitor_event(app, watcher, git_monitor, git_event);
                    }
                    app.process_notify(watcher, event);
                }
                Ok(Err(error)) => {
                    let fallback = watcher.roots.first().cloned().unwrap_or_default();
                    let mut integrity =
                        watcher::integrity_from_notify_error("filesystem", error, &fallback);
                    integrity.root = watcher.root_for(&integrity.root);
                    if integrity.level == IntegrityLevel::Lost {
                        watcher.schedule_recovery(watcher.root_for(&integrity.root));
                    }
                    app.process_integrity(integrity);
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
        let dropped = watcher.take_dropped_events();
        if dropped > 0 {
            app.process_integrity(IntegrityEvent {
                level: IntegrityLevel::Uncertain,
                source: "filesystem",
                summary: "filesystem event queue overflowed".into(),
                detail: format!("{dropped} native events were dropped before classification"),
                root: PathBuf::new(),
            });
        }
        watcher.seed_step(MAX_BASELINE_ENTRIES_PER_TICK);
        for event in watcher.recover_due() {
            app.process_integrity(event);
        }
        let omitted = watcher.take_snapshot_omissions();
        if omitted > 0 {
            app.process_integrity(IntegrityEvent {
                level: IntegrityLevel::Degraded,
                source: "filesystem",
                summary: "snapshot memory budget reached".into(),
                detail: format!("{omitted} text baselines were omitted; metadata remains visible"),
                root: PathBuf::new(),
            });
        }
        for event in git_monitor.drain() {
            process_git_monitor_event(app, watcher, git_monitor, event);
        }
        app.flush_pending(watcher);
        terminal.draw(|frame| ui::draw(frame, app, watcher, git_monitor.repository_count()))?;

        if event::poll(Duration::from_millis(80))?
            && let TerminalEvent::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(app, key.code, key.modifiers, &watcher.roots);
        }
    }
    Ok(())
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
    event: GitMonitorEvent,
) {
    match event {
        GitMonitorEvent::Activity(activity) => app.process_git(activity),
        GitMonitorEvent::Integrity(integrity) => app.process_integrity(integrity),
        GitMonitorEvent::RelatedWorktree(root) => match watcher.add_root(root.clone()) {
            Ok(Some(integrity)) => {
                app.process_integrity(integrity);
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

fn termination_requested() -> &'static Arc<AtomicBool> {
    TERMINATION_REQUESTED.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

#[cfg(unix)]
fn install_termination_handler() -> Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::flag;

    let requested = termination_requested();
    requested.store(false, Ordering::Relaxed);
    for signal in [SIGINT, SIGTERM, SIGHUP] {
        flag::register(signal, Arc::clone(requested))
            .with_context(|| format!("failed to register signal {signal}"))?;
    }
    Ok(())
}

#[cfg(windows)]
fn install_termination_handler() -> Result<()> {
    let requested = Arc::clone(termination_requested());
    requested.store(false, Ordering::Relaxed);
    ctrlc::set_handler(move || requested.store(true, Ordering::Relaxed))
        .context("failed to register console termination handler")
}

struct TerminalGuard {
    raw_mode: bool,
    alternate_screen: bool,
    restored: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        let mut guard = Self {
            raw_mode: false,
            alternate_screen: false,
            restored: false,
        };
        enable_raw_mode().context("failed to enter raw terminal mode")?;
        guard.raw_mode = true;
        execute!(stdout(), EnterAlternateScreen).context("failed to enter alternate screen")?;
        guard.alternate_screen = true;
        Ok(guard)
    }

    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        let mut errors = Vec::new();
        if self.raw_mode
            && let Err(error) = disable_raw_mode()
        {
            errors.push(format!("failed to leave raw terminal mode: {error}"));
        }
        if self.alternate_screen
            && let Err(error) = execute!(stdout(), LeaveAlternateScreen)
        {
            errors.push(format!("failed to leave alternate screen: {error}"));
        }
        if let Err(error) = execute!(stdout(), Show) {
            errors.push(format!("failed to restore cursor: {error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!(errors.join("; "))
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        previous(info);
    }));
}
