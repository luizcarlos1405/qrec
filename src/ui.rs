use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::{App, AppState, ControlsRow, FocusRegion};
use crate::timeline;

const FPS: f64 = 30.0;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_header(f, chunks[0]);
    draw_body(f, app, chunks[1]);
    draw_status_bar(f, app, chunks[2]);
}

fn draw_header(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " qrec",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" — Quick Recording"),
    ]))
    .block(Block::default().borders(Borders::BOTTOM));

    f.render_widget(title, area);
}

fn draw_body(f: &mut Frame, app: &App, area: Rect) {
    let controls_focus = app.focus == FocusRegion::Controls;
    let controls_highlight = if controls_focus {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let screen_prefix = if app.controls_row == ControlsRow::Screen && controls_focus {
        "▸"
    } else {
        " "
    };
    let screen_value = app.selected_screen().unwrap_or("(none)");
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

    let rec_prefix = if app.controls_row == ControlsRow::Record && controls_focus {
        "▸"
    } else {
        " "
    };
    let rec_label = match app.state {
        AppState::Recording => {
            format!("{}● REC  Recording... {:.1}s", rec_prefix, app.recording_elapsed_secs())
        }
        AppState::Rendering => format!("{} ⏳ Rendering...", rec_prefix),
        AppState::Ready => format!("{} [Start Recording]", rec_prefix),
    };
    let rec_style = if app.state == AppState::Recording {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
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
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} Screen:     ", screen_prefix),
                controls_highlight,
            ),
            Span::styled(format!("{:<30}", screen_value), Style::default().fg(Color::White)),
            Span::styled(screen_arrows, Style::default().fg(Color::DarkGray)),
        ])),
        inner_chunks[0],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} Microphone: ", mic_prefix),
                controls_highlight,
            ),
            Span::styled(format!("{:<30}", mic_value), Style::default().fg(Color::White)),
            Span::styled(mic_arrows, Style::default().fg(Color::DarkGray)),
        ])),
        inner_chunks[1],
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(rec_label, rec_style))),
        inner_chunks[2],
    );
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
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        };

        let ch = if i == app.selected_chunk { "▓" } else { "█" };

        spans.push(Span::styled(ch.repeat(vis_width), style));
        col += w;
    }

    if app.state == AppState::Recording {
        let elapsed = app.recording_elapsed_secs();
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
        AppState::Ready => Style::default().fg(Color::White).bg(Color::DarkGray),
    };

    let msg = if app.state == AppState::Recording {
        let elapsed = app.recording_elapsed_secs();
        format!(" ● REC {:.1}s — {}", elapsed, app.status_message)
    } else {
        format!(" {}", app.status_message)
    };

    f.render_widget(Paragraph::new(msg).style(style), area);
}
