use std::{path::Path, time::Duration};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap},
};

use crate::{
    app::App,
    model::{ChangeEvent, ChangeKind, DiffKind, TargetKind},
    watcher::WatchState,
};

const GREEN: Color = Color::Rgb(116, 214, 154);
const AMBER: Color = Color::Rgb(226, 180, 104);
const BLUE: Color = Color::Rgb(117, 164, 232);
const RED: Color = Color::Rgb(224, 116, 116);
const MUTED: Color = Color::Rgb(112, 112, 120);
const DIM: Color = Color::Rgb(70, 70, 77);
const PANEL: Color = Color::Rgb(18, 18, 21);
const SELECTED_PANEL: Color = Color::Rgb(27, 30, 31);
const FRESH_PANEL: Color = Color::Rgb(20, 24, 22);
const WORKSPACE_COLORS: [Color; 8] = [
    Color::Rgb(104, 190, 255),
    Color::Rgb(220, 140, 255),
    Color::Rgb(105, 210, 190),
    Color::Rgb(245, 180, 100),
    Color::Rgb(245, 130, 155),
    Color::Rgb(170, 190, 110),
    Color::Rgb(135, 155, 245),
    Color::Rgb(190, 160, 120),
];

pub fn draw(frame: &mut Frame, app: &App, watcher: &WatchState, git_repositories: usize) {
    let area = frame.area();
    let folders_height = if app.folders_open {
        watcher.roots.len().min(5) as u16 + 2
    } else {
        0
    };
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(folders_height),
        Constraint::Length(if app.search_mode { 3 } else { 2 }),
    ])
    .split(area);

    draw_header(frame, layout[0], app, watcher, git_repositories);
    draw_filters(frame, layout[1], app, watcher);

    if area.width >= 100 {
        let body = Layout::horizontal([Constraint::Percentage(44), Constraint::Percentage(56)])
            .spacing(1)
            .split(layout[2]);
        draw_timeline(frame, body[0], app, watcher);
        draw_detail(frame, body[1], app, watcher);
    } else {
        let body = Layout::vertical([Constraint::Percentage(52), Constraint::Percentage(48)])
            .spacing(1)
            .split(layout[2]);
        draw_timeline(frame, body[0], app, watcher);
        draw_detail(frame, body[1], app, watcher);
    }

    if app.folders_open {
        draw_tracked_folders(frame, layout[3], watcher);
    }
    draw_footer(frame, layout[4], app);
}

fn draw_header(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    watcher: &WatchState,
    git_repositories: usize,
) {
    let observer_level = app.observer_level();
    let live = if app.paused {
        format!(" PAUSED  +{} queued ", app.paused_events.len())
    } else if app.catching_up {
        " CATCHING UP ".to_string()
    } else if observer_level != crate::model::IntegrityLevel::Info {
        format!(" {} ", observer_level.label())
    } else {
        " ● LIVE ".to_string()
    };
    let live_color = if app.paused || app.catching_up {
        AMBER
    } else {
        integrity_color(observer_level)
    };

    let title = Line::from(vec![
        Span::styled(" ϟ ", Style::default().fg(Color::Black).bg(GREEN).bold()),
        Span::styled(" FLUX ", Style::default().fg(Color::White).bold()),
        Span::styled("workspace activity", Style::default().fg(MUTED)),
        Span::raw("  "),
        Span::styled(
            format!(
                "{} folders  ·  {git_repositories} repos",
                watcher.roots.len()
            ),
            Style::default().fg(Color::Rgb(155, 155, 160)),
        ),
    ]);
    let status = Line::from(Span::styled(
        live.clone(),
        Style::default().fg(live_color).bold(),
    ))
    .alignment(Alignment::Right);

    let columns = Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(live.len() as u16 + 2),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(title).block(bottom_border()), columns[0]);
    frame.render_widget(Paragraph::new(status).block(bottom_border()), columns[1]);
}

fn draw_tracked_folders(frame: &mut Frame, area: Rect, watcher: &WatchState) {
    let lines = watcher
        .roots
        .iter()
        .take(5)
        .enumerate()
        .map(|(index, root)| {
            Line::from(vec![
                workspace_span(&watcher.root_label(root), index),
                Span::raw("  "),
                Span::styled(root.display().to_string(), Style::default().fg(MUTED)),
            ])
        })
        .collect::<Vec<_>>();
    let hidden = watcher.roots.len().saturating_sub(lines.len());
    let title = if hidden == 0 {
        " TRACKED FOLDERS  ·  t hide ".to_string()
    } else {
        format!(" TRACKED FOLDERS  ·  +{hidden} more  ·  t hide ")
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::new()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DIM))
                .padding(Padding::horizontal(1))
                .style(Style::default().bg(PANEL)),
        ),
        area,
    );
}

fn draw_filters(frame: &mut Frame, area: Rect, app: &App, watcher: &WatchState) {
    let mut spans = vec![Span::styled(
        format!(" {} events  ", app.events.len()),
        Style::default().fg(Color::White).bold(),
    )];
    let workspace_label = app
        .workspace_filter
        .as_ref()
        .map(|root| watcher.root_label(root))
        .unwrap_or_else(|| "ALL".into());
    let workspace_color = app
        .workspace_filter
        .as_ref()
        .map(|root| workspace_color(watcher.root_index(root)))
        .unwrap_or(BLUE);
    spans.push(Span::styled(
        format!(" w:{workspace_label} "),
        Style::default().fg(Color::Black).bg(workspace_color).bold(),
    ));
    spans.push(Span::raw(" "));
    for (index, kind) in ChangeKind::ALL.iter().enumerate() {
        let active = app.enabled[index];
        let label = if area.width < 100 {
            format!(" {}{} ", index + 1, kind.symbol())
        } else {
            format!(" {}:{} {} ", index + 1, kind.symbol(), kind.label())
        };
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(if active { kind_color(*kind) } else { DIM })
                .add_modifier(if active {
                    Modifier::BOLD
                } else {
                    Modifier::DIM
                }),
        ));
    }
    if !app.search.is_empty() {
        spans.push(Span::styled(
            format!("  /{} ", app.search),
            Style::default().fg(Color::Black).bg(AMBER),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(
            Block::new()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(DIM)),
        ),
        area,
    );
}

fn draw_timeline(frame: &mut Frame, area: Rect, app: &App, watcher: &WatchState) {
    let visible = app.visible_indices();
    let selected_color = app.selected_event().map(event_color).unwrap_or(DIM);
    let inner_height = area.height.saturating_sub(2) as usize;
    let visible_rows = (inner_height / 2).max(1);
    let start = app.selected.saturating_sub(visible_rows.saturating_sub(1));
    let row_width = area.width.saturating_sub(2) as usize;
    let items = visible
        .iter()
        .skip(start)
        .take(visible_rows)
        .enumerate()
        .filter_map(|(offset, index)| app.events.get(*index).map(|event| (start + offset, event)))
        .map(|(visible_index, event)| {
            timeline_item(event, visible_index == app.selected, row_width, watcher)
        });

    let title = format!(
        " TIMELINE  {}/{} ",
        app.selected.saturating_add(1).min(visible.len()),
        visible.len()
    );
    frame.render_widget(
        List::new(items).block(
            Block::new()
                .title(title)
                .title_style(Style::default().fg(selected_color).bold())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(selected_color)),
        ),
        area,
    );
}

fn timeline_item(
    event: &ChangeEvent,
    selected: bool,
    width: usize,
    watcher: &WatchState,
) -> ListItem<'static> {
    let age = format_age(event.occurred_at.elapsed());
    let fresh = event.occurred_at.elapsed() < Duration::from_secs(3);
    let marker = if selected {
        "▶"
    } else if fresh {
        "●"
    } else {
        " "
    };
    let background = if selected {
        SELECTED_PANEL
    } else if fresh {
        FRESH_PANEL
    } else {
        Color::Reset
    };
    let path = event.path.to_string_lossy();
    let badge = event_badge(event);
    let badge_width = badge.chars().count() + 2;
    let workspace = (watcher.roots.len() > 1).then(|| watcher.root_label(&event.root));
    let workspace_width = workspace
        .as_ref()
        .map(|label| label.chars().count() + 3)
        .unwrap_or(0);
    let suffix_width = age.len() + 3;
    let path_width = width
        .saturating_sub(badge_width + workspace_width + suffix_width + 4)
        .max(8);
    let stats = match (event.lines_added, event.lines_removed) {
        (0, 0) => String::new(),
        (added, removed) => format!(" +{added} -{removed}"),
    };
    let freshness = if fresh { "  NEW" } else { "" };
    let context = event_context(event);
    let id_prefix = format!("  #{:<4} ", event.id);
    let context_width = width
        .saturating_sub(
            id_prefix.chars().count()
                + stats.chars().count()
                + freshness.chars().count()
                + age.chars().count()
                + 2,
        )
        .max(8);

    ListItem::new(Text::from(vec![
        Line::from(vec![
            Span::styled(
                format!("{marker} "),
                Style::default().fg(event_color(event)).bold(),
            ),
            workspace
                .map(|label| workspace_span(&label, watcher.root_index(&event.root)))
                .unwrap_or_else(|| Span::raw("")),
            badge_span(event),
            Span::raw(" "),
            Span::styled(
                truncate_middle(&path, path_width),
                Style::default()
                    .fg(if selected {
                        Color::White
                    } else {
                        Color::Rgb(190, 190, 195)
                    })
                    .bold(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                id_prefix,
                Style::default().fg(if selected { event_color(event) } else { DIM }),
            ),
            Span::styled(
                truncate_middle(&context, context_width),
                Style::default().fg(if selected { MUTED } else { DIM }),
            ),
            Span::styled(stats, Style::default().fg(MUTED)),
            Span::styled(freshness, Style::default().fg(event_color(event)).bold()),
            Span::styled(format!("  {age}"), Style::default().fg(DIM)),
        ]),
    ]))
    .style(Style::default().bg(background))
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App, watcher: &WatchState) {
    let Some(event) = app.selected_event() else {
        let empty = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::styled(
                "Waiting for filesystem activity…",
                Style::default().fg(MUTED),
            ),
            Line::from(""),
            Line::styled(
                "File, Git, and observer-integrity events appear here.",
                Style::default().fg(DIM),
            ),
        ]))
        .alignment(Alignment::Center)
        .block(panel(" CHANGE PREVIEW "));
        frame.render_widget(empty, area);
        return;
    };

    let root = compact_path(&event.root);
    let color = event_color(event);
    let workspace_label = watcher.root_label(&event.root);
    let workspace_index = watcher.root_index(&event.root);
    let target = match event.target {
        TargetKind::Directory => "directory",
        TargetKind::Repository => "repository",
        TargetKind::File => "file",
        TargetKind::Observer => "observer",
    };
    let mut lines = vec![
        Line::from(vec![
            if watcher.roots.len() > 1 {
                workspace_span(&workspace_label, workspace_index)
            } else {
                Span::raw("")
            },
            badge_span(event),
            Span::styled(
                format!("  #{:<4} {target}", event.id),
                Style::default().fg(MUTED),
            ),
            Span::styled(
                event
                    .integrity_level
                    .map(|level| format!("  {}", level.label()))
                    .unwrap_or_default(),
                Style::default().fg(color).bold(),
            ),
            Span::styled(
                format!("  {}", format_age(event.occurred_at.elapsed())),
                Style::default().fg(DIM),
            ),
        ]),
        Line::styled(
            event.path.to_string_lossy().to_string(),
            Style::default().fg(Color::White).bold().underlined(),
        ),
        Line::from(vec![
            Span::styled(
                "WORKSPACE  ",
                Style::default().fg(workspace_color(workspace_index)).bold(),
            ),
            Span::styled(workspace_label, Style::default().fg(Color::White).bold()),
            Span::styled("  ROOT  ", Style::default().fg(color).bold()),
            Span::styled(root, Style::default().fg(MUTED)),
        ]),
    ];
    if let Some(previous) = &event.previous_path {
        lines.push(Line::from(vec![
            Span::styled(previous.to_string_lossy(), Style::default().fg(MUTED)),
            Span::styled("  →  ", Style::default().fg(color).bold()),
            Span::styled(
                event.path.to_string_lossy(),
                Style::default().fg(color).bold(),
            ),
        ]));
    }
    lines.push(Line::from(""));

    if event.diff.is_empty() {
        let detail = event.detail.as_deref().unwrap_or(match event.kind {
            ChangeKind::Delete => "removed from the workspace",
            ChangeKind::Rename => "moved without textual changes",
            ChangeKind::Git => "repository state changed",
            ChangeKind::Integrity => "observation state changed",
            _ if event.target == TargetKind::Directory => "directory event",
            _ => "metadata changed",
        });
        lines.push(Line::from(vec![
            Span::styled("DETAIL  ", Style::default().fg(color).bold()),
            Span::styled(detail, Style::default().fg(Color::Rgb(180, 180, 185))),
        ]));
        if event.size > 0 {
            lines.push(Line::styled(
                format!("{} bytes", event.size),
                Style::default().fg(DIM),
            ));
        }
    } else {
        lines.push(Line::from(vec![
            Span::styled("DIFF  ", Style::default().fg(color).bold()),
            Span::styled(
                format!(" +{} ", event.lines_added),
                Style::default().fg(Color::Black).bg(GREEN).bold(),
            ),
            Span::raw("  "),
            Span::styled(
                format!(" -{} ", event.lines_removed),
                Style::default().fg(Color::Black).bg(RED).bold(),
            ),
        ]));
        lines.push(Line::from(""));
        for diff_line in &event.diff {
            let (symbol, color) = match diff_line.kind {
                DiffKind::Add => ("+", GREEN),
                DiffKind::Remove => ("-", RED),
                DiffKind::Equal => (" ", Color::Rgb(126, 126, 132)),
            };
            let number = match diff_line.kind {
                DiffKind::Remove => diff_line.old_number,
                _ => diff_line.new_number,
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>4} ", number.unwrap_or(0)),
                    Style::default().fg(DIM),
                ),
                Span::styled(format!("{symbol} "), Style::default().fg(color).bold()),
                Span::styled(diff_line.text.clone(), Style::default().fg(color)),
            ]));
        }
        if let Some(detail) = &event.detail {
            lines.push(Line::styled(
                format!("… {detail}"),
                Style::default().fg(AMBER),
            ));
        }
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(
                Block::new()
                    .title(Line::from(vec![
                        Span::styled(
                            " SELECTED ",
                            Style::default().fg(Color::Black).bg(color).bold(),
                        ),
                        if watcher.roots.len() > 1 {
                            workspace_span(&watcher.root_label(&event.root), workspace_index)
                        } else {
                            Span::raw("")
                        },
                        Span::styled(
                            format!("  {}  #{} ", event_badge(event), event.id),
                            Style::default().fg(color).bold(),
                        ),
                    ]))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(color))
                    .padding(Padding::new(1, 1, 1, 1))
                    .style(Style::default().bg(PANEL)),
            ),
        area,
    );
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    if app.search_mode {
        let search = Line::from(vec![
            Span::styled(" / ", Style::default().fg(Color::Black).bg(AMBER).bold()),
            Span::styled(&app.search, Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(AMBER)),
            Span::styled("  enter accept  esc cancel", Style::default().fg(DIM)),
        ]);
        frame.render_widget(Paragraph::new(search).block(top_border()), area);
    } else {
        let help = if area.width < 100 {
            Line::from(vec![
                key("j/k"),
                hint(" move  "),
                key("/"),
                hint(" find  "),
                key("w"),
                hint(" root  "),
                key("1-6"),
                hint(" types  "),
                key("t"),
                hint(" folders  "),
                key("spc"),
                hint(" pause  "),
                key("q"),
                hint(" quit"),
            ])
        } else {
            Line::from(vec![
                key("j/k"),
                hint(" move  "),
                key("/"),
                hint(" search  "),
                key("w"),
                hint(" workspace  "),
                key("1-6"),
                hint(" filters  "),
                key("t"),
                hint(" folders  "),
                key("space"),
                hint(" pause  "),
                key("c"),
                hint(" clear  "),
                key("q"),
                hint(" quit"),
            ])
        };
        let status = app
            .status
            .as_deref()
            .unwrap_or("no notifications · local only");
        if area.width < 100 {
            frame.render_widget(Paragraph::new(help).block(top_border()), area);
        } else {
            let columns = Layout::horizontal([
                Constraint::Min(20),
                Constraint::Length(status.len() as u16 + 2),
            ])
            .split(area);
            frame.render_widget(Paragraph::new(help).block(top_border()), columns[0]);
            frame.render_widget(
                Paragraph::new(
                    Line::styled(status, Style::default().fg(DIM)).alignment(Alignment::Right),
                )
                .block(top_border()),
                columns[1],
            );
        }
    }
}

fn panel(title: &'static str) -> Block<'static> {
    Block::new()
        .title(title)
        .title_style(Style::default().fg(MUTED).bold())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM))
        .padding(Padding::new(1, 1, 1, 1))
        .style(Style::default().bg(PANEL))
}

fn top_border() -> Block<'static> {
    Block::new()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(DIM))
}

fn bottom_border() -> Block<'static> {
    Block::new()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(DIM))
}

fn key(value: &'static str) -> Span<'static> {
    Span::styled(
        format!(" {value} "),
        Style::default()
            .fg(Color::Rgb(190, 190, 195))
            .bg(Color::Rgb(35, 35, 40)),
    )
}

fn hint(value: &'static str) -> Span<'static> {
    Span::styled(value, Style::default().fg(DIM))
}

fn kind_color(kind: ChangeKind) -> Color {
    match kind {
        ChangeKind::Create => GREEN,
        ChangeKind::Modify => AMBER,
        ChangeKind::Rename => BLUE,
        ChangeKind::Delete => RED,
        ChangeKind::Git => Color::Rgb(196, 143, 255),
        ChangeKind::Integrity => Color::Rgb(255, 109, 109),
    }
}

fn workspace_color(index: usize) -> Color {
    WORKSPACE_COLORS[index % WORKSPACE_COLORS.len()]
}

fn workspace_span(label: &str, index: usize) -> Span<'static> {
    Span::styled(
        format!(" {} ", truncate_middle(label, 16)),
        Style::default()
            .fg(Color::Black)
            .bg(workspace_color(index))
            .bold(),
    )
}

fn event_badge(event: &ChangeEvent) -> String {
    event.kind.label().to_string()
}

fn badge_span(event: &ChangeEvent) -> Span<'static> {
    Span::styled(
        format!(" {} ", event_badge(event)),
        Style::default()
            .fg(Color::Black)
            .bg(event_color(event))
            .bold(),
    )
}

fn event_context(event: &ChangeEvent) -> String {
    if let Some(previous) = &event.previous_path {
        return format!("{} → {}", previous.display(), event.path.display());
    }
    match event.target {
        TargetKind::Repository => event
            .detail
            .clone()
            .unwrap_or_else(|| compact_path(&event.root)),
        TargetKind::Observer => {
            let severity = event
                .integrity_level
                .map(|level| level.label())
                .unwrap_or("INFO");
            format!(
                "{severity} · {}",
                event
                    .detail
                    .clone()
                    .unwrap_or_else(|| compact_path(&event.root))
            )
        }
        TargetKind::Directory => format!("directory · {}", compact_path(&event.root)),
        TargetKind::File => event
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| compact_path(&event.root)),
    }
}

fn event_color(event: &ChangeEvent) -> Color {
    event
        .integrity_level
        .map(integrity_color)
        .unwrap_or_else(|| kind_color(event.kind))
}

fn integrity_color(level: crate::model::IntegrityLevel) -> Color {
    match level {
        crate::model::IntegrityLevel::Info => GREEN,
        crate::model::IntegrityLevel::Degraded => AMBER,
        crate::model::IntegrityLevel::Uncertain => Color::Rgb(255, 145, 85),
        crate::model::IntegrityLevel::Lost => RED,
    }
}

fn compact_path(path: &Path) -> String {
    let components: Vec<_> = path.components().collect();
    if components.len() <= 3 {
        return path.display().to_string();
    }
    format!(
        "…/{}",
        components[components.len() - 2..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    )
}

fn truncate_middle(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let tail = value
        .chars()
        .rev()
        .take(max.saturating_sub(2))
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("…/{tail}")
}

fn format_age(age: Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0..=2 => "now".into(),
        3..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}
