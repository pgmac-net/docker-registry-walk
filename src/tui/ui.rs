#![allow(dead_code)]

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use crate::ops::diff::DiffStatus;

use super::app::{
    App, Focus, InspectModal, LoadState, Modal, OwnerChoice, SPINNER, ghcr_owner_choices,
};
use super::detail;
use super::input::{InputState, cursor_spans, input_scroll_skip};
use super::jsonview::{RowMeta, close_bracket};

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

    // The Inspect modal is drawn from a mutable borrow so it can record its
    // viewport height (for paging) each frame; handle it before the shared
    // match over the other modals.
    if let Modal::Inspect(m) = &mut app.modal {
        draw_inspect_modal(frame, m, area);
        return;
    }

    match &app.modal {
        Modal::Confirm { message, .. } => draw_confirm_modal(frame, message.clone(), area),
        Modal::Input {
            prompt,
            input,
            on_confirm,
        } => {
            draw_input_modal(frame, prompt, input, on_confirm.is_secret(), area);
        }
        Modal::RegistrySelect { selected_idx } => {
            draw_registry_select_modal(frame, app, *selected_idx, area)
        }
        Modal::Inspect(_) => {} // handled above
        Modal::LayerDiff(m) => draw_layer_diff_modal(frame, m, area),
        Modal::Help { scroll } => draw_help_modal(frame, *scroll, area),
        Modal::SearchPicker {
            input,
            results,
            selected,
            searching,
        } => draw_search_picker_modal(frame, input, results, *selected, *searching, area),
        Modal::ArtifactoryPicker {
            filter,
            repos,
            selected,
            loading,
        } => {
            let f = filter.buffer.to_lowercase();
            let rows: Vec<String> = repos
                .iter()
                .filter(|r| f.is_empty() || r.key.to_lowercase().contains(&f))
                .map(|r| format!("{} ({})", r.key, r.repo_type))
                .collect();
            draw_filter_picker_modal(
                frame,
                filter,
                &rows,
                *selected,
                *loading,
                ARTIFACTORY_PICKER_LABELS,
                area,
            );
        }
        Modal::GhcrOwnerPicker {
            input,
            owners,
            selected,
            loading,
        } => {
            // Rows come from the same helper the key handler selects with, so
            // the row opened is always the row highlighted.
            let rows: Vec<String> = ghcr_owner_choices(&input.buffer, owners)
                .iter()
                .map(OwnerChoice::label)
                .collect();
            draw_filter_picker_modal(
                frame,
                input,
                &rows,
                *selected,
                *loading,
                GHCR_OWNER_PICKER_LABELS,
                area,
            );
        }
        Modal::GhcrPicker {
            filter,
            packages,
            selected,
            loading,
        } => {
            let f = filter.buffer.to_lowercase();
            let rows: Vec<String> = packages
                .iter()
                .filter(|p| f.is_empty() || p.to_lowercase().contains(&f))
                .cloned()
                .collect();
            draw_filter_picker_modal(
                frame,
                filter,
                &rows,
                *selected,
                *loading,
                GHCR_PICKER_LABELS,
                area,
            );
        }
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
        " docker-registry-walk v{}  │  [{}]  {}{}",
        env!("CARGO_PKG_VERSION"),
        app.registry_name,
        app.registry_url,
        switch_hint
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

/// Centered popup rect. `width` scales with the terminal (`width_pct` of
/// `area.width`) but never shrinks below `min_width`, and both dimensions
/// are clamped to fit within `area` (minus a 4-cell margin). This lets
/// popups grow on large terminals instead of staying pinned to their
/// design-minimum size.
fn popup_rect(area: Rect, min_width: u16, width_pct: u16, height: u16) -> Rect {
    let pct_width = area.width.saturating_mul(width_pct) / 100;
    let width = min_width.max(pct_width).min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}

fn draw_confirm_modal(frame: &mut Frame, message: String, area: Rect) {
    let modal_area = popup_rect(area, 50, 40, 5);

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(" Confirm ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let text = format!("{message}\n\n[y] Confirm  [n/Esc] Cancel");
    let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true });

    frame.render_widget(p, modal_area);
}

/// Character echoed in place of each character of a secret.
const MASK_CHAR: char = '•';

fn draw_input_modal(frame: &mut Frame, prompt: &str, input: &InputState, secret: bool, area: Rect) {
    let modal_area = popup_rect(area, 60, 50, 5);
    let width = modal_area.width;

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(format!(" {prompt} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    // Inner width minus the two border columns.
    let inner_width = (width as usize).saturating_sub(2);
    let skip = input_scroll_skip(input.cursor, inner_width);
    let visible: String = input.buffer.chars().skip(skip).take(inner_width).collect();
    let col = input.cursor - skip;

    // Mask *after* slicing so the scroll window and cursor column above are
    // computed from the real buffer and need no adjustment. The replacement is
    // 1:1 per character, so `col` still points at the right cell.
    let visible = if secret {
        MASK_CHAR.to_string().repeat(visible.chars().count())
    } else {
        visible
    };

    // A masked 600-char JWT is unreadable, but a length confirms a paste landed.
    let footer = if secret {
        format!(
            "({} chars)  [Enter] Confirm  [Esc] Cancel",
            input.buffer.chars().count()
        )
    } else {
        "[Enter] Confirm  [Esc] Cancel".to_owned()
    };

    let lines = vec![
        Line::from(cursor_spans(&visible, col)),
        Line::raw(""),
        Line::raw(footer),
    ];
    let p = Paragraph::new(lines).block(block);

    frame.render_widget(p, modal_area);
}

fn draw_search_picker_modal(
    frame: &mut Frame,
    input: &InputState,
    results: &[String],
    selected: usize,
    searching: bool,
    area: Rect,
) {
    // 4-cell margin + 3-row filter box + 2 list border rows, matching
    // popup_rect's own clamping — leaves however many rows the terminal
    // actually has room for instead of capping at a fixed 10.
    let max_rows = area.height.saturating_sub(9).max(1);
    let result_rows = (results.len() as u16).min(max_rows);
    let height = if results.is_empty() {
        5
    } else {
        result_rows + 5
    };
    let modal_area = popup_rect(area, 70, 60, height);
    let width = modal_area.width;

    frame.render_widget(Clear, modal_area);

    let title = if searching {
        " Docker Hub Search ⠸ "
    } else {
        " Docker Hub Search "
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(modal_area);

    let inner_width = (width as usize).saturating_sub(2);
    let skip = input_scroll_skip(input.cursor, inner_width);
    let visible: String = input.buffer.chars().skip(skip).take(inner_width).collect();
    let col = input.cursor - skip;
    let search_input = Paragraph::new(Line::from(cursor_spans(&visible, col))).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(search_input, chunks[0]);

    if results.is_empty() {
        if !searching && !input.buffer.is_empty() {
            let msg = Paragraph::new(" No results").block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            frame.render_widget(msg, chunks[1]);
        } else {
            let hint = Paragraph::new(" [↑↓/jk] navigate  [Enter] open  [Esc] cancel").block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            frame.render_widget(hint, chunks[1]);
        }
        return;
    }

    let items: Vec<ListItem> = results.iter().map(|r| ListItem::new(r.as_str())).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title(" Results  [↑↓/jk] navigate  [Enter] open  [Esc] cancel ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(HIGHLIGHT_STYLE)
        .highlight_symbol("▶ ");
    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(Some(selected.min(results.len().saturating_sub(1))));
    frame.render_stateful_widget(list, chunks[1], &mut list_state);
}

/// The labels that distinguish one filtered picker from another.
#[derive(Clone, Copy)]
struct PickerLabels {
    /// Title of the filter input box. A spinner is appended while loading.
    title: &'static str,
    /// Title of the results list, carrying the key hints.
    list_title: &'static str,
    /// Shown when the list is empty and the fetch has finished.
    empty: &'static str,
}

const ARTIFACTORY_PICKER_LABELS: PickerLabels = PickerLabels {
    title: " Artifactory Repositories ",
    list_title: " Repo-keys  [↑↓/jk] navigate  [Enter] open  [Esc] cancel ",
    empty: " No repositories found",
};

const GHCR_OWNER_PICKER_LABELS: PickerLabels = PickerLabels {
    title: " GHCR Owner ",
    list_title: " Owners  [↑↓] navigate  [Enter] browse  [Esc] cancel  — or type any owner ",
    empty: " Type an owner to browse",
};

const GHCR_PICKER_LABELS: PickerLabels = PickerLabels {
    title: " GHCR Packages ",
    list_title: " Packages  [↑↓/jk] navigate  [Enter] browse  [Esc] cancel ",
    empty: " No packages found",
};

/// Renderer shared by the two one-shot, locally-filtered pickers.
///
/// The Artifactory repo-key picker and the GHCR package picker differ only in
/// their labels and in how a row is formatted — layout, filter input,
/// scrolling and empty state are identical, so they live here once. `rows` is
/// already filtered by the caller, which is also where the per-type row
/// formatting happens.
fn draw_filter_picker_modal(
    frame: &mut Frame,
    filter: &InputState,
    filtered: &[String],
    selected: usize,
    loading: bool,
    labels: PickerLabels,
    area: Rect,
) {
    let max_rows = area.height.saturating_sub(9).max(1);
    let result_rows = (filtered.len() as u16).min(max_rows);
    // The empty state needs 6, not 5: the layout below gives the filter input
    // 3 rows and the rest to the list, and a bordered block only starts having
    // interior rows at height 3. At 5 the message ("Loading…" / "No … found")
    // rendered into a zero-row interior and was invisible — which a GHCR
    // listing makes obvious, since it can spend a minute paging the GitHub API
    // with nothing else on screen to say so.
    let height = if filtered.is_empty() {
        6
    } else {
        result_rows + 5
    };
    let modal_area = popup_rect(area, 70, 60, height);
    let width = modal_area.width;

    frame.render_widget(Clear, modal_area);

    let title = if loading {
        format!("{}⠸ ", labels.title)
    } else {
        labels.title.to_owned()
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(modal_area);

    let inner_width = (width as usize).saturating_sub(2);
    let skip = input_scroll_skip(filter.cursor, inner_width);
    let visible: String = filter.buffer.chars().skip(skip).take(inner_width).collect();
    let col = filter.cursor - skip;
    let filter_input = Paragraph::new(Line::from(cursor_spans(&visible, col))).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(filter_input, chunks[0]);

    if filtered.is_empty() {
        let msg = if loading { " Loading…" } else { labels.empty };
        let p = Paragraph::new(msg).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(p, chunks[1]);
        return;
    }

    let items: Vec<ListItem> = filtered
        .iter()
        .map(|row| ListItem::new(row.as_str()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title(labels.list_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(HIGHLIGHT_STYLE)
        .highlight_symbol("▶ ");
    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(Some(selected.min(filtered.len().saturating_sub(1))));
    frame.render_stateful_widget(list, chunks[1], &mut list_state);
}

fn draw_registry_select_modal(frame: &mut Frame, app: &App, selected_idx: usize, area: Rect) {
    let n = app.profiles.len();
    let height = n as u16 + 4;
    let modal_area = popup_rect(area, 60, 50, height);

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

fn draw_inspect_modal(frame: &mut Frame, m: &mut InspectModal, area: Rect) {
    let width = area.width.saturating_sub(4);
    let height = area.height.saturating_sub(4);
    let x = area.x + 2;
    let y = area.y + 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

    let match_info = if m.has_matches() {
        format!("  ({}/{})", m.search.current + 1, m.search.matches.len())
    } else if !m.search.query.is_empty() {
        "  (no matches)".to_owned()
    } else {
        String::new()
    };
    let block = Block::default()
        .title(format!(" Inspect: {}{match_info} ", m.title))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    // Reserve the bottom row for the search bar / key hints.
    let content_h = inner.height.saturating_sub(1);
    let content = Rect::new(inner.x, inner.y, inner.width, content_h);
    let footer = Rect::new(
        inner.x,
        inner.y + content_h,
        inner.width,
        inner.height.saturating_sub(content_h),
    );

    // Tell the modal how tall the content area is, so cursor paging and
    // scroll-follow use the real viewport.
    m.set_viewport(content_h as usize);

    let query = m.search.query.to_lowercase();
    let visible: Vec<Line> = m
        .visible
        .iter()
        .enumerate()
        .skip(m.scroll)
        .take(content_h as usize)
        .map(|(vi, &abs)| {
            let is_cursor = vi == m.cursor;
            let is_match = !query.is_empty() && m.search.matches.contains(&abs);
            inspect_line(
                &m.lines[abs],
                m.rows[abs],
                m.collapsed[abs],
                is_cursor,
                &query,
                is_match,
            )
        })
        .collect();

    frame.render_widget(Paragraph::new(visible), content);

    // Footer: live search entry, or a compact key hint + position.
    if m.search.active {
        let inner_w = (footer.width as usize).saturating_sub(1);
        let skip = input_scroll_skip(m.search.input.cursor, inner_w);
        let text: String = m
            .search
            .input
            .buffer
            .chars()
            .skip(skip)
            .take(inner_w)
            .collect();
        let col = m.search.input.cursor - skip;
        let mut spans = vec![Span::styled("/", Style::default().fg(Color::Yellow))];
        spans.extend(cursor_spans(&text, col));
        frame.render_widget(Paragraph::new(Line::from(spans)), footer);
    } else {
        let pos = if m.visible.is_empty() {
            0
        } else {
            (m.cursor + 1) * 100 / m.visible.len()
        };
        let hint = format!("↑↓ move · ␣ fold · / search · n/N next · ? help · q close   {pos}%");
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            ))),
            footer,
        );
    }
}

/// Render one inspect row: gutter fold glyph, the JSON text (coloured, with
/// a `⋯` marker when collapsed), and cursor / search highlighting.
fn inspect_line(
    line: &str,
    row: RowMeta,
    collapsed: bool,
    is_cursor: bool,
    query: &str,
    is_match: bool,
) -> Line<'static> {
    let gutter = if row.opener {
        if collapsed { "▸ " } else { "▾ " }
    } else {
        "  "
    };
    // Collapsed openers show a fold marker standing in for the hidden body.
    let marker = if row.opener && collapsed {
        close_bracket(line)
            .map(|c| format!(" ⋯ {c}"))
            .unwrap_or_default()
    } else {
        String::new()
    };

    // Cursor row: uniform highlight over the whole line (token colour is
    // dropped for the single selected row, matching list-selection style).
    if is_cursor {
        let text = format!("{gutter}{line}{marker}");
        return Line::from(Span::styled(text, HIGHLIGHT_STYLE));
    }

    let mut spans = vec![Span::styled(
        gutter.to_owned(),
        Style::default().fg(Color::DarkGray),
    )];
    if is_match {
        spans.extend(highlight_spans(line, query));
    } else {
        spans.extend(colorize_json_line(line).spans);
    }
    if !marker.is_empty() {
        spans.push(Span::styled(marker, Style::default().fg(Color::DarkGray)));
    }
    Line::from(spans)
}

/// Split `line` on case-insensitive occurrences of `query`, styling the hits.
///
/// Byte offsets are taken from the lowercased haystack and reused on the
/// original; ASCII-lowercasing preserves length, so for non-ASCII lines
/// (rare in registry JSON) fall back to plain rendering to avoid slicing
/// mid-codepoint.
fn highlight_spans(line: &str, query: &str) -> Vec<Span<'static>> {
    if !line.is_ascii() {
        return vec![Span::raw(line.to_owned())];
    }
    let hay = line.to_lowercase();
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = hay[cursor..].find(query) {
        let start = cursor + rel;
        let end = start + query.len();
        if start > cursor {
            spans.push(Span::raw(line[cursor..start].to_owned()));
        }
        spans.push(Span::styled(
            line[start..end].to_owned(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        cursor = end;
    }
    if cursor < line.len() {
        spans.push(Span::raw(line[cursor..].to_owned()));
    }
    spans
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
    let modal_area = popup_rect(area, 62, 50, 32);

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
        header("Inspect viewer  (inside the JSON overlay)"),
        Line::from(vec![key("↑↓ / j k"), sep(), desc("Move cursor")]),
        Line::from(vec![key("PgUp/PgDn"), sep(), desc("Page up / down")]),
        Line::from(vec![key("g / G"), sep(), desc("Jump to top / bottom")]),
        Line::from(vec![
            key("Space/Enter"),
            sep(),
            desc("Fold / unfold node at cursor"),
        ]),
        Line::from(vec![key("← / →"), sep(), desc("Collapse / expand node")]),
        Line::from(vec![key("H / L"), sep(), desc("Collapse all / expand all")]),
        Line::from(vec![key("/"), sep(), desc("Search JSON text")]),
        Line::from(vec![key("n / N"), sep(), desc("Next / previous match")]),
        Line::from(vec![
            key("?"),
            sep(),
            desc("This help (returns to the viewer on close)"),
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
        Line::from(vec![
            key("Backspace / u"),
            sep(),
            desc("Up a level: repo-key picker (Artifactory) / owner picker (GHCR)"),
        ]),
        Line::from(vec![]),
        header("Docker Hub Search  (opens automatically on Docker Hub)"),
        Line::from(vec![
            key("↑ / ↓"),
            sep(),
            desc("Select result (j/k not available — they insert text)"),
        ]),
        Line::from(vec![key("Enter"), sep(), desc("Browse selected repo")]),
        Line::from(vec![key("Esc"), sep(), desc("Close without searching")]),
        Line::from(vec![]),
        header("General"),
        Line::from(vec![key("?"), sep(), desc("This help screen")]),
        Line::from(vec![key("q / Esc"), sep(), desc("Quit")]),
        Line::from(vec![key("Ctrl-C"), sep(), desc("Force quit")]),
        Line::from(vec![]),
        Line::from(vec![
            Span::styled(
                "Version",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            sep(),
            desc(concat!("v", env!("CARGO_PKG_VERSION"))),
        ]),
    ];

    let visible_h = inner.height as usize;
    let max_scroll = lines.len().saturating_sub(visible_h);
    let clamped = scroll.min(max_scroll);
    let visible: Vec<Line> = lines.into_iter().skip(clamped).take(visible_h).collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::app::{ConfirmAction, InputAction};
    use super::*;
    use crate::config::{RegistryProfile, RegistryType};
    use crate::registry::ArtifactoryRepo;

    fn make_app() -> App {
        let profile = RegistryProfile {
            name: "test".to_owned(),
            url: "http://localhost:5000".to_owned(),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        App::new(vec![profile], 0)
    }

    fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn search_picker_scrolls_last_selection_into_view() {
        let mut app = make_app();
        let results: Vec<String> = (0..30).map(|i| format!("repo-{i}")).collect();
        app.modal = Modal::SearchPicker {
            input: InputState::default(),
            results,
            selected: 29,
            searching: false,
        };

        // Small enough that the list still can't show all 30 rows at once,
        // so this keeps exercising scroll-to-selection (not just "grew
        // enough to fit everything").
        let content = render_to_string(&mut app, 100, 20);
        assert!(
            content.contains("repo-29"),
            "selected (last) result must be scrolled into view"
        );
    }

    #[test]
    fn artifactory_picker_scrolls_last_selection_into_view() {
        let mut app = make_app();
        let repos: Vec<ArtifactoryRepo> = (0..30)
            .map(|i| ArtifactoryRepo {
                key: format!("docker-local-{i}"),
                repo_type: "LOCAL".to_owned(),
                url: String::new(),
                package_type: "Docker".to_owned(),
            })
            .collect();
        app.modal = Modal::ArtifactoryPicker {
            filter: InputState::default(),
            repos,
            selected: 29,
            loading: false,
        };

        let content = render_to_string(&mut app, 100, 20);
        assert!(
            content.contains("docker-local-29"),
            "selected (last) repo-key must be scrolled into view"
        );
    }

    /// The GHCR picker shares its renderer with the Artifactory one, so this
    /// pins that the shared code still scrolls a selection into view — and
    /// that a nested package name survives rendering intact.
    #[test]
    fn ghcr_picker_scrolls_last_selection_into_view() {
        let mut app = make_app();
        let packages: Vec<String> = (0..30).map(|i| format!("owner/core/pkg-{i}")).collect();
        app.modal = Modal::GhcrPicker {
            filter: InputState::default(),
            packages,
            selected: 29,
            loading: false,
        };

        let content = render_to_string(&mut app, 100, 20);
        assert!(
            content.contains("owner/core/pkg-29"),
            "selected (last) package must be scrolled into view"
        );
    }

    /// The renderer and the key handler both build rows from
    /// `ghcr_owner_choices`, so what is drawn is what `Enter` opens. This pins
    /// that the typed row reaches the screen.
    #[test]
    fn ghcr_owner_picker_renders_the_typed_owner_and_matching_suggestions() {
        let mut app = make_app();
        app.modal = Modal::GhcrOwnerPicker {
            input: input_with("home"),
            owners: vec!["pgmac".to_owned(), "Homebrew".to_owned()],
            selected: 0,
            loading: false,
        };

        let content = render_to_string(&mut app, 100, 20);
        assert!(content.contains(r#"Use "home""#), "typed row must render");
        assert!(content.contains("Homebrew"), "matching suggestion kept");
        assert!(
            !content.contains("pgmac"),
            "non-matching suggestion filtered"
        );
    }

    /// Without a `read:org` PAT the suggestion list is empty, and the picker is
    /// then nothing but a text box — so its prompt has to be visible.
    #[test]
    fn ghcr_owner_picker_empty_state_prompts_for_typing() {
        let mut app = make_app();
        app.modal = Modal::GhcrOwnerPicker {
            input: InputState::default(),
            owners: Vec::new(),
            selected: 0,
            loading: false,
        };

        assert!(
            render_to_string(&mut app, 100, 30).contains("Type an owner"),
            "an empty owner list must still tell the user what to do"
        );
    }

    /// A GHCR listing can spend a minute paging the GitHub API, so the empty
    /// state is the whole screen for that time. It has to actually render: at
    /// the old height the message landed in a bordered box with zero interior
    /// rows and was invisible.
    #[test]
    fn picker_empty_state_messages_are_visible() {
        let mut app = make_app();

        app.modal = Modal::GhcrPicker {
            filter: InputState::default(),
            packages: Vec::new(),
            selected: 0,
            loading: true,
        };
        assert!(
            render_to_string(&mut app, 100, 30).contains("Loading…"),
            "the loading message must be visible while the fetch is out"
        );

        app.modal = Modal::GhcrPicker {
            filter: input_with("nomatch"),
            packages: vec!["homebrew/core/git".to_owned()],
            selected: 0,
            loading: false,
        };
        assert!(
            render_to_string(&mut app, 100, 30).contains("No packages found"),
            "a filter matching nothing must say so rather than render blank"
        );
    }

    /// The filter is applied by the caller now that the renderer is shared;
    /// this pins that the GHCR arm actually wires it up.
    #[test]
    fn ghcr_picker_renders_only_filtered_rows() {
        let mut app = make_app();
        app.modal = Modal::GhcrPicker {
            filter: input_with("sql"),
            packages: vec![
                "homebrew/core/git".to_owned(),
                "homebrew/core/sqldiff".to_owned(),
            ],
            selected: 0,
            loading: false,
        };

        let content = render_to_string(&mut app, 100, 20);
        assert!(content.contains("homebrew/core/sqldiff"));
        assert!(!content.contains("homebrew/core/git"));
    }

    #[test]
    fn search_picker_shows_more_than_ten_rows_on_a_tall_terminal() {
        let mut app = make_app();
        let results: Vec<String> = (0..20).map(|i| format!("repo-{i}")).collect();
        app.modal = Modal::SearchPicker {
            input: InputState::default(),
            results,
            selected: 0,
            searching: false,
        };

        // Tall terminal, selection pinned at the top (no scroll needed) —
        // an item past the old fixed 10-row cap must still be visible.
        let content = render_to_string(&mut app, 100, 40);
        assert!(
            content.contains("repo-15"),
            "picker should show more than 10 rows when the terminal has room"
        );
    }

    #[test]
    fn artifactory_picker_shows_more_than_ten_rows_on_a_tall_terminal() {
        let mut app = make_app();
        let repos: Vec<ArtifactoryRepo> = (0..20)
            .map(|i| ArtifactoryRepo {
                key: format!("docker-local-{i}"),
                repo_type: "LOCAL".to_owned(),
                url: String::new(),
                package_type: "Docker".to_owned(),
            })
            .collect();
        app.modal = Modal::ArtifactoryPicker {
            filter: InputState::default(),
            repos,
            selected: 0,
            loading: false,
        };

        let content = render_to_string(&mut app, 100, 40);
        assert!(
            content.contains("docker-local-15"),
            "picker should show more than 10 rows when the terminal has room"
        );
    }

    #[test]
    fn pickers_render_without_panic_on_a_small_terminal() {
        let mut app = make_app();
        let results: Vec<String> = (0..30).map(|i| format!("repo-{i}")).collect();
        app.modal = Modal::SearchPicker {
            input: InputState::default(),
            results,
            selected: 29,
            searching: false,
        };
        let content = render_to_string(&mut app, 40, 12);
        assert!(
            content.contains("repo-29"),
            "selection should still be visible on a small terminal"
        );
    }

    #[test]
    fn all_popup_modals_render_without_panic_on_a_small_terminal() {
        for modal in [
            Modal::Confirm {
                message: "Delete this?".to_owned(),
                on_confirm: ConfirmAction::DeleteManifest {
                    repo: "r".to_owned(),
                    tag: "t".to_owned(),
                },
            },
            Modal::Input {
                prompt: "Name:".to_owned(),
                input: InputState::default(),
                on_confirm: InputAction::BrowseRepo,
            },
            Modal::Help { scroll: 0 },
            Modal::GhcrPicker {
                filter: InputState::default(),
                packages: vec!["homebrew/core/git".to_owned()],
                selected: 0,
                loading: false,
            },
            Modal::GhcrOwnerPicker {
                input: InputState::default(),
                owners: vec!["Homebrew".to_owned()],
                selected: 0,
                loading: false,
            },
            Modal::Inspect(Box::new(InspectModal::new(
                "img:tag".to_owned(),
                "{\n  \"config\": {\n    \"digest\": \"sha256:abc\"\n  }\n}"
                    .lines()
                    .map(str::to_owned)
                    .collect(),
            ))),
        ] {
            let mut app = make_app();
            app.modal = modal;
            // Just assert it doesn't panic on a very small terminal.
            let _ = render_to_string(&mut app, 20, 8);
        }
    }

    fn input_with(text: &str) -> InputState {
        let mut input = InputState::default();
        for c in text.chars() {
            input.insert(c);
        }
        input
    }

    #[test]
    fn input_modal_masks_secret_action() {
        let mut app = make_app();
        app.modal = Modal::Input {
            prompt: "Password for u:".to_owned(),
            input: input_with("hunter2secret"),
            on_confirm: InputAction::EnterPassword {
                profile_name: "test".to_owned(),
                username: "u".to_owned(),
            },
        };

        let content = render_to_string(&mut app, 80, 12);
        assert!(
            !content.contains("hunter2secret"),
            "secret must never be echoed to the screen"
        );
        assert!(
            content.contains(&MASK_CHAR.to_string().repeat("hunter2secret".len())),
            "every character of the secret must render as the mask char"
        );
        assert!(
            content.contains("(13 chars)"),
            "a length hint confirms a paste landed"
        );
    }

    #[test]
    fn input_modal_shows_plaintext_for_non_secret_action() {
        let mut app = make_app();
        app.modal = Modal::Input {
            prompt: "Repo:".to_owned(),
            input: input_with("nginx"),
            on_confirm: InputAction::BrowseRepo,
        };

        let content = render_to_string(&mut app, 80, 12);
        assert!(
            content.contains("nginx"),
            "non-secret input must stay visible"
        );
        assert!(
            !content.contains(MASK_CHAR),
            "non-secret input must not be masked"
        );
    }
}
