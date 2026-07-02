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

pub fn draw(frame: &mut Frame, app: &App, watcher: &WatchState, git_repositories: usize) {
    let area = frame.area();
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(if app.search_mode { 3 } else { 2 }),
    ])
    .split(area);

    draw_header(frame, layout[0], app, watcher, git_repositories);
    draw_filters(frame, layout[1], app);

    if area.width >= 100 {
        let body = Layout::horizontal([Constraint::Percentage(44), Constraint::Percentage(56)])
            .spacing(1)
            .split(layout[2]);
        draw_timeline(frame, body[0], app);
        draw_detail(frame, body[1], app);
    } else {
        let body = Layout::vertical([Constraint::Percentage(52), Constraint::Percentage(48)])
            .spacing(1)
            .split(layout[2]);
        draw_timeline(frame, body[0], app);
        draw_detail(frame, body[1], app);
    }

    draw_footer(frame, layout[3], app);
}

fn draw_header(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    watcher: &WatchState,
    git_repositories: usize,
) {
    let roots = watcher
        .roots
        .iter()
        .map(|root| compact_path(root))
        .collect::<Vec<_>>()
        .join("  ·  ");
    let live = if app.paused {
        format!(" PAUSED  +{} queued ", app.paused_events.len())
    } else {
        " ● LIVE ".to_string()
    };
    let live_color = if app.paused { AMBER } else { GREEN };

    let title = Line::from(vec![
        Span::styled(" ϟ ", Style::default().fg(Color::Black).bg(GREEN).bold()),
        Span::styled(" FLUX ", Style::default().fg(Color::White).bold()),
        Span::styled("filesystem activity", Style::default().fg(MUTED)),
        Span::raw("  "),
        Span::styled(roots, Style::default().fg(Color::Rgb(155, 155, 160))),
        Span::styled(
            format!("  {git_repositories} git"),
            Style::default().fg(BLUE),
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

fn draw_filters(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(
        format!(" {} events  ", app.events.len()),
        Style::default().fg(Color::White).bold(),
    )];
    for (index, kind) in ChangeKind::ALL.iter().enumerate() {
        let active = app.enabled[index];
        spans.push(Span::styled(
            format!(" {}:{} {} ", index + 1, kind.symbol(), kind.label()),
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

fn draw_timeline(frame: &mut Frame, area: Rect, app: &App) {
    let visible = app.visible_indices();
    let inner_height = area.height.saturating_sub(2) as usize;
    let start = app.selected.saturating_sub(inner_height.saturating_sub(1));
    let items = visible
        .iter()
        .skip(start)
        .take(inner_height)
        .enumerate()
        .filter_map(|(offset, index)| app.events.get(*index).map(|event| (start + offset, event)))
        .map(|(visible_index, event)| timeline_item(event, visible_index == app.selected));

    let title = format!(
        " TIMELINE  {}/{} ",
        app.selected.saturating_add(1).min(visible.len()),
        visible.len()
    );
    frame.render_widget(
        List::new(items).block(
            Block::new()
                .title(title)
                .title_style(Style::default().fg(MUTED).bold())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DIM)),
        ),
        area,
    );
}

fn timeline_item(event: &ChangeEvent, selected: bool) -> ListItem<'static> {
    let age = format_age(event.occurred_at.elapsed());
    let path = event.path.to_string_lossy();
    let prefix = if selected { "▌" } else { " " };
    let background = if selected {
        Color::Rgb(29, 31, 31)
    } else {
        Color::Reset
    };
    let stats = match (event.lines_added, event.lines_removed) {
        (0, 0) => String::new(),
        (added, removed) => format!(" +{added} -{removed}"),
    };
    ListItem::new(Line::from(vec![
        Span::styled(
            prefix,
            Style::default().fg(kind_color(event.kind)).bg(background),
        ),
        Span::styled(
            format!(" {} ", event.kind.symbol()),
            Style::default()
                .fg(kind_color(event.kind))
                .bg(background)
                .bold(),
        ),
        Span::styled(
            truncate_middle(&path, 34),
            Style::default()
                .fg(if selected {
                    Color::White
                } else {
                    Color::Rgb(180, 180, 184)
                })
                .bg(background),
        ),
        Span::styled(stats, Style::default().fg(MUTED).bg(background)),
        Span::styled(format!("  {age}"), Style::default().fg(DIM).bg(background)),
    ]))
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App) {
    let Some(event) = app.selected_event() else {
        let empty = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::styled(
                "Waiting for filesystem activity…",
                Style::default().fg(MUTED),
            ),
            Line::from(""),
            Line::styled(
                "Creates, edits, moves, and deletes appear here.",
                Style::default().fg(DIM),
            ),
        ]))
        .alignment(Alignment::Center)
        .block(panel(" CHANGE PREVIEW "));
        frame.render_widget(empty, area);
        return;
    };

    let root = compact_path(&event.root);
    let target = match event.target {
        TargetKind::Directory => "directory",
        TargetKind::Repository => "repository",
        TargetKind::File => "file",
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" {} ", event.kind.label()),
                Style::default()
                    .fg(Color::Black)
                    .bg(kind_color(event.kind))
                    .bold(),
            ),
            Span::styled(
                format!("  {target}  #{}", event.id),
                Style::default().fg(MUTED),
            ),
        ]),
        Line::styled(
            event.path.to_string_lossy().to_string(),
            Style::default().fg(Color::White).bold(),
        ),
        Line::styled(root, Style::default().fg(DIM)),
    ];
    if let Some(previous) = &event.previous_path {
        lines.push(Line::from(vec![
            Span::styled(previous.to_string_lossy(), Style::default().fg(MUTED)),
            Span::styled("  →  ", Style::default().fg(BLUE)),
            Span::styled(event.path.to_string_lossy(), Style::default().fg(BLUE)),
        ]));
    }
    lines.push(Line::from(""));

    if event.diff.is_empty() {
        let detail = event.detail.as_deref().unwrap_or(match event.kind {
            ChangeKind::Delete => "removed from the workspace",
            ChangeKind::Rename => "moved without textual changes",
            ChangeKind::Git => "repository state changed",
            _ if event.target == TargetKind::Directory => "directory event",
            _ => "metadata changed",
        });
        lines.push(Line::styled(detail, Style::default().fg(MUTED)));
        if event.size > 0 {
            lines.push(Line::styled(
                format!("{} bytes", event.size),
                Style::default().fg(DIM),
            ));
        }
    } else {
        lines.push(Line::from(vec![
            Span::styled(
                format!("+{}", event.lines_added),
                Style::default().fg(GREEN).bold(),
            ),
            Span::raw("  "),
            Span::styled(
                format!("-{}", event.lines_removed),
                Style::default().fg(RED).bold(),
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
            .block(panel(" CHANGE PREVIEW ")),
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
                key("1-5"),
                hint(" types  "),
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
                key("1-5"),
                hint(" filters  "),
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
