use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, AppState, ControlsRow, FocusRegion};
use crate::timeline;

const FPS: f64 = 30.0;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_header(f, app, chunks[0]);
    draw_body(f, app, chunks[1]);
    draw_status_bar(f, app, chunks[2]);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let title_line = Line::from(vec![
        Span::styled(
            " qrec",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" — Quick Recording"),
    ]);

    let dim = Style::default().fg(Color::DarkGray);
    let key_style = Style::default().fg(Color::Yellow);
    let sep = Span::styled("  │  ", dim);

    let shortcuts_line = match app.state {
        AppState::Recording => Line::from(vec![
            Span::styled(" r", key_style),
            Span::styled(" Stop", dim),
        ]),
        AppState::Rendering => Line::from(vec![Span::styled(" Rendering in progress…", dim)]),
        AppState::Ready | AppState::Exited => match app.focus {
            FocusRegion::Controls => Line::from(vec![
                Span::styled(" j/k", key_style),
                Span::styled(" Navigate", dim),
                sep.clone(),
                Span::styled(" h/l", key_style),
                Span::styled(" Change", dim),
                sep.clone(),
                Span::styled(" r", key_style),
                Span::styled(" Record", dim),
                sep.clone(),
                Span::styled(" d", key_style),
                Span::styled(" Discard", dim),
                sep.clone(),
                Span::styled(" e", key_style),
                Span::styled(" Render", dim),
                sep.clone(),
                Span::styled(" P", key_style),
                Span::styled(" Preview", dim),
                sep.clone(),
                Span::styled(" Tab", key_style),
                Span::styled(" Timeline", dim),
            ]),
            FocusRegion::Timeline => Line::from(vec![
                Span::styled(" h/l", key_style),
                Span::styled(" Select", dim),
                sep.clone(),
                Span::styled(" H/L", key_style),
                Span::styled(" Reorder", dim),
                sep.clone(),
                Span::styled(" i/o", key_style),
                Span::styled(" Zoom", dim),
                sep.clone(),
                Span::styled(" d", key_style),
                Span::styled(" Delete", dim),
                sep.clone(),
                Span::styled(" p", key_style),
                Span::styled(" Preview", dim),
                sep.clone(),
                Span::styled(" r", key_style),
                Span::styled(" Record", dim),
                sep.clone(),
                Span::styled(" e", key_style),
                Span::styled(" Render", dim),
                sep.clone(),
                Span::styled(" P", key_style),
                Span::styled(" Preview All", dim),
                sep.clone(),
                Span::styled(" Tab", key_style),
                Span::styled(" Controls", dim),
            ]),
        },
    };

    let header = Paragraph::new(vec![title_line, shortcuts_line])
        .block(Block::default().borders(Borders::BOTTOM));

    f.render_widget(header, area);
}

fn draw_body(f: &mut Frame, app: &App, area: Rect) {
    let controls_focus = app.focus == FocusRegion::Controls;
    let controls_highlight = if controls_focus {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let screen_prefix = if app.controls_row == ControlsRow::Screen && controls_focus {
        "▸"
    } else {
        " "
    };
    let screen_value = app.selected_screen_display();
    let screen_arrows = if app.controls_row == ControlsRow::Screen && controls_focus {
        " ◄ ►"
    } else {
        ""
    };

    let mic_prefix = if app.controls_row == ControlsRow::Microphone && controls_focus {
        "▸"
    } else {
        " "
    };
    let mic_value = app.selected_microphone_display();
    let mic_arrows = if app.controls_row == ControlsRow::Microphone && controls_focus {
        " ◄ ►"
    } else {
        ""
    };

    let rec_indicator = match app.state {
        AppState::Recording => Some(Line::from(Span::styled(
            format!("  ● REC  Recording... {:.1}s", app.recording_elapsed_secs),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))),
        AppState::Rendering => Some(Line::from(Span::styled(
            "  ⏳ Rendering...",
            Style::default(),
        ))),
        AppState::Ready | AppState::Exited => None,
    };

    let controls_block = Block::default()
        .title(" Controls ")
        .borders(Borders::ALL)
        .border_style(if controls_focus {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        });

    let inner = controls_block.inner(area);
    let inner_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} Screen:     ", screen_prefix),
                controls_highlight,
            ),
            Span::styled(
                format!("{:<30}", screen_value),
                Style::default().fg(Color::White),
            ),
            Span::styled(screen_arrows, Style::default().fg(Color::DarkGray)),
        ])),
        inner_chunks[0],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{} Microphone: ", mic_prefix), controls_highlight),
            Span::styled(
                format!("{:<30}", mic_value),
                Style::default().fg(Color::White),
            ),
            Span::styled(mic_arrows, Style::default().fg(Color::DarkGray)),
        ])),
        inner_chunks[1],
    );

    if let Some(rec_line) = rec_indicator {
        let indicator_area = Rect {
            x: area.x,
            y: area.y + 3,
            width: area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(rec_line), indicator_area);
    }

    f.render_widget(controls_block, area);

    let timeline_area = Rect {
        x: area.x,
        y: area.y + 4,
        width: area.width,
        height: area.height.saturating_sub(4),
    };

    draw_timeline_section(f, app, timeline_area);
}

fn draw_timeline_section(f: &mut Frame, app: &App, area: Rect) {
    let timeline_focus = app.focus == FocusRegion::Timeline;

    let block = Block::default()
        .title(" Timeline ")
        .borders(Borders::ALL)
        .border_style(if timeline_focus {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        });

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.config.chunks.is_empty() && app.state != AppState::Recording {
        let empty = Paragraph::new("No chunks recorded")
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: false });
        f.render_widget(empty, inner);
        return;
    }

    let mut spans = Vec::new();
    let mut col = 0;
    let scroll = app.viewport_scroll;
    let view_w = inner.width as usize;

    for (i, chunk) in app.config.chunks.iter().enumerate() {
        let w = timeline::chunk_char_width(chunk.duration_secs, FPS, app.frames_per_char);
        let chunk_start = col;
        let chunk_end = col + w;

        if chunk_end <= scroll || chunk_start >= scroll + view_w {
            col += w;
            continue;
        }

        let vis_start = chunk_start.saturating_sub(scroll);
        let vis_end = (chunk_end).saturating_sub(scroll).min(view_w);
        let vis_width = vis_end.saturating_sub(vis_start);

        if vis_width == 0 {
            col += w;
            continue;
        }

        let style = if i == app.selected_chunk {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        };

        let ch = if i == app.selected_chunk {
            "█"
        } else {
            "▓"
        };

        if i > 0 {
            spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
        }

        spans.push(Span::styled(ch.repeat(vis_width), style));
        col += w;
    }

    if app.state == AppState::Recording {
        let elapsed = app.recording_elapsed_secs;
        let rec_width = timeline::chunk_char_width(elapsed, FPS, app.frames_per_char).max(1);
        let pulse = "▒".repeat(rec_width);
        spans.push(Span::styled(
            pulse,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }

    let line = Line::from(spans);
    f.render_widget(Paragraph::new(line), inner);
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let style = match app.state {
        AppState::Recording => Style::default()
            .fg(Color::Black)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD),
        AppState::Rendering => Style::default().fg(Color::Black).bg(Color::Yellow),
        AppState::Ready | AppState::Exited => Style::default().fg(Color::White).bg(Color::DarkGray),
    };

    let msg = if app.state == AppState::Recording {
        let elapsed = app.recording_elapsed_secs;
        format!(" ● REC {:.1}s — {}", elapsed, app.status_message)
    } else {
        format!(" {}", app.status_message)
    };

    f.render_widget(Paragraph::new(msg).style(style), area);
}
