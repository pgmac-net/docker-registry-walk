#![allow(dead_code)]

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use crate::ops::diff::DiffStatus;

use super::app::{App, Focus, LoadState, Modal, SPINNER};
use super::detail;

const HIGHLIGHT_STYLE: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::Cyan)
    .add_modifier(Modifier::BOLD);

const ACTIVE_BORDER: Style = Style::new().fg(Color::Cyan);
const INACTIVE_BORDER: Style = Style::new().fg(Color::DarkGray);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(5),    // main panels
            Constraint::Length(3), // details
            Constraint::Length(1), // keybindings
        ])
        .split(area);

    draw_title(frame, app, vertical[0]);
    draw_main_panels(frame, app, vertical[1]);
    draw_details(frame, app, vertical[2]);
    draw_keybindings(frame, app, vertical[3]);

    match &app.modal {
        Modal::Confirm { message, .. } => draw_confirm_modal(frame, message.clone(), area),
        Modal::Input {
            prompt,
            value,
            cursor,
            ..
        } => draw_input_modal(frame, prompt.clone(), value.clone(), *cursor, area),
        Modal::RegistrySelect { selected_idx } => {
            draw_registry_select_modal(frame, app, *selected_idx, area)
        }
        Modal::Inspect(m) => draw_inspect_modal(frame, &m.title, &m.lines, m.scroll, area),
        Modal::LayerDiff(m) => draw_layer_diff_modal(frame, m, area),
        Modal::Help { scroll } => draw_help_modal(frame, *scroll, area),
        Modal::None => {}
    }
}

fn draw_title(frame: &mut Frame, app: &App, area: Rect) {
    let switch_hint = if app.profiles.len() > 1 {
        "  [R] switch"
    } else {
        ""
    };
    let title = format!(
        " docker-registry-walk  │  [{}]  {}{}",
        app.registry_name, app.registry_url, switch_hint
    );
    let p = Paragraph::new(title).style(
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(p, area);
}

fn draw_main_panels(frame: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    draw_repos(frame, app, cols[0]);
    draw_tags(frame, app, cols[1]);
}

fn draw_repos(frame: &mut Frame, app: &mut App, area: Rect) {
    let border_style = if app.focus == Focus::Repos {
        ACTIVE_BORDER
    } else {
        INACTIVE_BORDER
    };

    let spinner_char = SPINNER[app.spinner_tick % SPINNER.len()];
    let title = match &app.repo_load {
        LoadState::Loading => format!(" Repositories {spinner_char} "),
        LoadState::Error(_) => " Repositories ✗ ".to_owned(),
        LoadState::Idle => {
            let count = app.repos.len();
            if app.filter_mode == Some(Focus::Repos) {
                format!(" Repos / {} ", app.repo_filter)
            } else if !app.repo_filter.is_empty() {
                format!(" Repositories [{count}] (filtered) ")
            } else {
                format!(" Repositories ({count}) ")
            }
        }
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = if let LoadState::Error(msg) = &app.repo_load {
        vec![
            ListItem::new(format!("✗ {msg}"))
                .style(ratatui::style::Style::default().fg(ratatui::style::Color::Red)),
        ]
    } else {
        app.repos
            .iter()
            .map(|r| ListItem::new(r.as_str()))
            .collect()
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(HIGHLIGHT_STYLE)
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut app.repos_state);
}

fn draw_tags(frame: &mut Frame, app: &mut App, area: Rect) {
    let border_style = if app.focus == Focus::Tags {
        ACTIVE_BORDER
    } else {
        INACTIVE_BORDER
    };

    let spinner_char = SPINNER[app.spinner_tick % SPINNER.len()];
    let sort_label = app.tag_sort.label();
    let title = match &app.tag_load {
        LoadState::Loading => format!(" Tags {spinner_char} "),
        LoadState::Error(_) => " Tags ✗ ".to_owned(),
        LoadState::Idle => {
            let count = app.tags.len();
            if app.filter_mode == Some(Focus::Tags) {
                format!(" Tags / {} ", app.tag_filter)
            } else if !app.tag_filter.is_empty() {
                format!(" Tags [{count}] (filtered) [{sort_label}] ")
            } else {
                format!(" Tags ({count}) [{sort_label}] ")
            }
        }
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = if let LoadState::Error(msg) = &app.tag_load {
        vec![
            ListItem::new(format!("✗ {msg}"))
                .style(ratatui::style::Style::default().fg(ratatui::style::Color::Red)),
        ]
    } else {
        app.tags.iter().map(|t| ListItem::new(t.as_str())).collect()
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(HIGHLIGHT_STYLE)
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut app.tags_state);
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.focus == Focus::Detail {
        ACTIVE_BORDER
    } else {
        INACTIVE_BORDER
    };

    let spinner_char = SPINNER[app.spinner_tick % SPINNER.len()];
    let title = match &app.detail_load {
        LoadState::Loading => format!(" Details {spinner_char} "),
        LoadState::Error(_) => " Details ✗ ".to_owned(),
        LoadState::Idle => " Details ".to_owned(),
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = match &app.detail {
        Some(d) => detail::render_lines(d),
        None => {
            let msg = if let Some(s) = app.status_text() {
                s.to_owned()
            } else {
                match &app.detail_load {
                    LoadState::Loading => String::new(),
                    LoadState::Error(e) => format!("Error: {e}"),
                    LoadState::Idle => " Select a tag to view details".to_owned(),
                }
            };
            vec![Line::raw(msg)]
        }
    };

    let visible_h = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(visible_h);
    let scroll = app.detail_scroll.min(max_scroll);
    let visible: Vec<Line> = lines.into_iter().skip(scroll).take(visible_h).collect();

    let p = Paragraph::new(visible);
    frame.render_widget(p, inner);
}

fn draw_keybindings(frame: &mut Frame, app: &App, area: Rect) {
    let spans = if app.filter_mode.is_some() {
        Line::from(vec![
            Span::styled(" Typing filter", Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" clear  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(" confirm "),
        ])
    } else if app.focus == Focus::Detail {
        Line::from(vec![
            Span::styled(" Tab", Style::default().fg(Color::Cyan)),
            Span::raw(" focus  "),
            Span::styled("↑↓", Style::default().fg(Color::Cyan)),
            Span::raw(" scroll  "),
            Span::styled("c", Style::default().fg(Color::Cyan)),
            Span::raw(" copy  "),
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(" quit "),
        ])
    } else {
        let mut parts = vec![
            Span::styled(" Tab", Style::default().fg(Color::Cyan)),
            Span::raw(" focus  "),
            Span::styled("↑↓", Style::default().fg(Color::Cyan)),
            Span::raw(" nav  "),
            Span::styled("/", Style::default().fg(Color::Cyan)),
            Span::raw(" filter  "),
            Span::styled("s", Style::default().fg(Color::Cyan)),
            Span::raw(" sort  "),
            Span::styled("i", Style::default().fg(Color::Cyan)),
            Span::raw(" inspect  "),
            Span::styled("c", Style::default().fg(Color::Cyan)),
            Span::raw(" copy  "),
            Span::styled("C", Style::default().fg(Color::Cyan)),
            Span::raw(" copy-to  "),
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::raw(" retag  "),
            Span::styled("D", Style::default().fg(Color::Cyan)),
            Span::raw(" diff  "),
            Span::styled("e", Style::default().fg(Color::Cyan)),
            Span::raw(" export  "),
            Span::styled("P", Style::default().fg(Color::Yellow)),
            Span::raw(" prune  "),
            Span::styled("d", Style::default().fg(Color::Red)),
            Span::raw(" delete  "),
        ];
        if app.profiles.len() > 1 {
            parts.push(Span::styled("R", Style::default().fg(Color::Magenta)));
            parts.push(Span::raw(" switch  "));
        }
        parts.push(Span::styled("?", Style::default().fg(Color::Cyan)));
        parts.push(Span::raw(" help  "));
        parts.push(Span::styled("q", Style::default().fg(Color::Cyan)));
        parts.push(Span::raw(" quit "));
        Line::from(parts)
    };
    let p = Paragraph::new(spans).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(p, area);
}

fn draw_confirm_modal(frame: &mut Frame, message: String, area: Rect) {
    let width = 50u16.min(area.width.saturating_sub(4));
    let height = 5u16;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(" Confirm ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let text = format!("{message}\n\n[y] Confirm  [n/Esc] Cancel");
    let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true });

    frame.render_widget(p, modal_area);
}

fn draw_input_modal(frame: &mut Frame, prompt: String, value: String, cursor: usize, area: Rect) {
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = 5u16;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(format!(" {prompt} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let split = value.char_indices().nth(cursor).map(|(i, _)| i).unwrap_or(value.len());
    let display = format!("{}{}{}", &value[..split], '|', &value[split..]);
    let text = format!("{display}\n\n[Enter] Confirm  [Esc] Cancel");
    let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true });

    frame.render_widget(p, modal_area);
}

fn draw_registry_select_modal(frame: &mut Frame, app: &App, selected_idx: usize, area: Rect) {
    let n = app.profiles.len();
    let height = (n as u16 + 4).min(area.height.saturating_sub(4));
    let width = 60u16.min(area.width.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(" Switch Registry ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));

    if n <= 1 {
        let text = "No other registries configured.\n\n[Esc] Cancel";
        let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true });
        frame.render_widget(p, modal_area);
        return;
    }

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    let items: Vec<ListItem> = app
        .profiles
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let active = if i == app.active_profile_idx {
                "* "
            } else {
                "  "
            };
            ListItem::new(format!("{active}[{}]  {}", p.name, p.url))
        })
        .collect();

    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(Some(selected_idx));

    let list = List::new(items)
        .highlight_style(HIGHLIGHT_STYLE)
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, inner, &mut list_state);
}

fn draw_inspect_modal(frame: &mut Frame, title: &str, lines: &[String], scroll: usize, area: Rect) {
    let width = area.width.saturating_sub(4);
    let height = area.height.saturating_sub(4);
    let x = area.x + 2;
    let y = area.y + 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(format!(" Inspect: {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    let visible_h = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(visible_h);
    let scroll = scroll.min(max_scroll);

    let visible: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .take(visible_h)
        .map(|l| colorize_json_line(l))
        .collect();

    frame.render_widget(Paragraph::new(visible), inner);

    // Scroll indicator in bottom-right of border.
    if lines.len() > visible_h {
        let pct = (scroll * 100).checked_div(max_scroll).unwrap_or(100);
        let indicator = format!(" {pct}% ");
        let ind_x = modal_area.x + modal_area.width.saturating_sub(indicator.len() as u16 + 1);
        let ind_y = modal_area.y + modal_area.height.saturating_sub(1);
        if ind_x > modal_area.x && ind_y < modal_area.y + modal_area.height {
            let ind_area = Rect::new(ind_x, ind_y, indicator.len() as u16, 1);
            frame.render_widget(
                Paragraph::new(indicator).style(Style::default().fg(Color::DarkGray)),
                ind_area,
            );
        }
    }
}

/// Heuristic line-by-line JSON syntax colouring.
fn colorize_json_line(line: &str) -> Line<'static> {
    let trimmed = line.trim_start();

    // Key-value pair: "key": value
    if let Some(colon_pos) = trimmed.find("\": ") {
        let indent = &line[..line.len() - trimmed.len()];
        let key_end = colon_pos + 1; // include closing quote
        let key_part = format!("{indent}{}", &trimmed[..key_end]);
        let rest = &trimmed[colon_pos + 3..]; // after ": "

        let value_span = if rest.starts_with('"') {
            Span::styled(rest.to_owned(), Style::default().fg(Color::Green))
        } else if rest == "true" || rest == "false" || rest == "null" {
            Span::styled(rest.to_owned(), Style::default().fg(Color::Magenta))
        } else if rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '-')
        {
            Span::styled(rest.to_owned(), Style::default().fg(Color::Yellow))
        } else {
            Span::raw(rest.to_owned())
        };

        return Line::from(vec![
            Span::styled(key_part, Style::default().fg(Color::Cyan)),
            Span::raw(": "),
            value_span,
        ]);
    }

    // Section separator line.
    if trimmed.starts_with("──") {
        return Line::from(Span::styled(
            line.to_owned(),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Line::raw(line.to_owned())
}

fn draw_layer_diff_modal(frame: &mut Frame, m: &crate::tui::app::LayerDiffModal, area: Rect) {
    let width = area.width.saturating_sub(4);
    let height = area.height.saturating_sub(4);
    let x = area.x + 2;
    let y = area.y + 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(format!(" Diff: {}  vs  {} ", m.tag_a, m.tag_b))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    let legend = Line::from(vec![
        Span::styled("+ added  ", Style::default().fg(Color::Green)),
        Span::styled("- removed  ", Style::default().fg(Color::Red)),
        Span::styled("= unchanged", Style::default().fg(Color::DarkGray)),
    ]);

    // Build content lines: legend + blank + layer rows.
    let mut content: Vec<Line> = vec![legend, Line::raw("")];
    for layer in &m.layers {
        let (prefix, color) = match layer.status {
            DiffStatus::Added => ("+", Color::Green),
            DiffStatus::Removed => ("-", Color::Red),
            DiffStatus::Unchanged => ("=", Color::DarkGray),
        };
        let size_kb = layer.size / 1024;
        let line = format!("{prefix} {}  ({size_kb} KB)", layer.digest);
        content.push(Line::from(Span::styled(line, Style::default().fg(color))));
    }

    let visible_h = inner.height as usize;
    let max_scroll = content.len().saturating_sub(visible_h);
    let scroll = m.scroll.min(max_scroll);
    let visible: Vec<Line> = content.into_iter().skip(scroll).take(visible_h).collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

fn draw_help_modal(frame: &mut Frame, scroll: usize, area: Rect) {
    let width = 62u16.min(area.width.saturating_sub(4));
    let height = 32u16.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(" Keybindings — ? or Esc to close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    let key = |k: &'static str| {
        Span::styled(
            k,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    };
    let sep = || Span::raw("  ");
    let desc = |d: &'static str| Span::raw(d);
    let header = |h: &'static str| {
        Line::from(vec![Span::styled(
            h,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )])
    };

    let lines: Vec<Line> = vec![
        header("Navigation"),
        Line::from(vec![key("↑ / k"), sep(), desc("Move up")]),
        Line::from(vec![key("↓ / j"), sep(), desc("Move down")]),
        Line::from(vec![
            key("Tab"),
            sep(),
            desc("Cycle panel (Repos → Tags → Detail)"),
        ]),
        Line::from(vec![
            key("Enter"),
            sep(),
            desc("Move focus to Tags when in Repos"),
        ]),
        Line::from(vec![]),
        header("Filter"),
        Line::from(vec![key("/"), sep(), desc("Start filter in current panel")]),
        Line::from(vec![key("Esc / Enter"), sep(), desc("Exit filter mode")]),
        Line::from(vec![]),
        header("Image operations  (require a tag selected)"),
        Line::from(vec![key("c"), sep(), desc("Copy pull URL to clipboard")]),
        Line::from(vec![
            key("C"),
            sep(),
            desc("Copy image to another registry/repo"),
        ]),
        Line::from(vec![
            key("r"),
            sep(),
            desc("Retag — push manifest under a new tag"),
        ]),
        Line::from(vec![
            key("d"),
            sep(),
            desc("Delete tag (requires delete enabled)"),
        ]),
        Line::from(vec![
            key("i"),
            sep(),
            desc("Inspect raw manifest & config JSON"),
        ]),
        Line::from(vec![
            key("e"),
            sep(),
            desc("Export image as OCI tar archive"),
        ]),
        Line::from(vec![
            key("D"),
            sep(),
            desc("Diff layers against another tag"),
        ]),
        Line::from(vec![]),
        header("Repository operations  (require a repo selected)"),
        Line::from(vec![
            key("P"),
            sep(),
            desc("Prune digest-only (untagged) manifests"),
        ]),
        Line::from(vec![]),
        header("Tags panel"),
        Line::from(vec![
            key("s"),
            sep(),
            desc("Cycle tag sort order (↑ / ↓ name)"),
        ]),
        Line::from(vec![]),
        header("Registry"),
        Line::from(vec![key("R"), sep(), desc("Switch registry (in-app)")]),
        Line::from(vec![]),
        header("General"),
        Line::from(vec![key("?"), sep(), desc("This help screen")]),
        Line::from(vec![key("q / Ctrl-C"), sep(), desc("Quit")]),
    ];

    let visible_h = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(visible_h);
    let clamped = scroll.min(max_scroll);
    let visible: Vec<Line> = lines.into_iter().skip(clamped).take(visible_h).collect();

    frame.render_widget(Paragraph::new(visible), inner);
}
