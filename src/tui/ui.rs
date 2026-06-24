#![allow(dead_code)]

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use super::app::{App, Focus, Modal};

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

    if let Modal::Confirm { message, .. } = &app.modal {
        draw_modal(frame, message.clone(), area);
    }
}

fn draw_title(frame: &mut Frame, app: &App, area: Rect) {
    let title = format!(
        " docker-registry-walk  │  {}  │  {}",
        app.registry_name, app.registry_url
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

    let block = Block::default()
        .title(" Repositories ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = app
        .repos
        .iter()
        .map(|r| ListItem::new(r.as_str()))
        .collect();

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

    let block = Block::default()
        .title(" Tags ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = app.tags.iter().map(|t| ListItem::new(t.as_str())).collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(HIGHLIGHT_STYLE)
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut app.tags_state);
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
    let status = app.status_text().unwrap_or("");
    let content = match (app.selected_repo(), app.selected_tag()) {
        (Some(repo), Some(tag)) => format!("{repo}:{tag}"),
        (Some(repo), None) => repo.to_owned(),
        _ => String::new(),
    };

    let text = if status.is_empty() {
        content
    } else {
        format!("{content}  │  {status}")
    };

    let block = Block::default()
        .title(" Details ")
        .borders(Borders::ALL)
        .border_style(INACTIVE_BORDER);

    let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true });

    frame.render_widget(p, area);
}

fn draw_keybindings(frame: &mut Frame, _app: &App, area: Rect) {
    let spans = Line::from(vec![
        Span::styled(" Tab", Style::default().fg(Color::Cyan)),
        Span::raw(" focus  "),
        Span::styled("↑↓", Style::default().fg(Color::Cyan)),
        Span::raw(" navigate  "),
        Span::styled("Enter", Style::default().fg(Color::Cyan)),
        Span::raw(" select  "),
        Span::styled("d", Style::default().fg(Color::Red)),
        Span::raw(" delete  "),
        Span::styled("q", Style::default().fg(Color::Cyan)),
        Span::raw(" quit "),
    ]);
    let p = Paragraph::new(spans).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(p, area);
}

fn draw_modal(frame: &mut Frame, message: String, area: Rect) {
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
