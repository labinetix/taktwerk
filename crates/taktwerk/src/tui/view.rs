//! Drawing the monitor.

use std::time::{Duration, SystemTime};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row as TableRow, Table, TableState};

use super::app::App;
use super::value;

/// The engine status code as a word.
pub fn status_name(status: Option<i64>) -> &'static str {
    match status {
        Some(0) => "Init",
        Some(1) => "Running",
        Some(2) => "Faulted",
        Some(3) => "Stopped",
        Some(_) => "?",
        None => "-",
    }
}

/// Age of a value: `12 ms`, `3.4 s`, `95 s`.
pub fn age(now: SystemTime, stamp: Option<SystemTime>) -> String {
    let Some(stamp) = stamp else {
        return "-".to_owned();
    };
    let d = now.duration_since(stamp).unwrap_or(Duration::ZERO);
    if d < Duration::from_secs(1) {
        format!("{} ms", d.as_millis())
    } else if d < Duration::from_secs(60) {
        format!("{:.1} s", d.as_secs_f64())
    } else {
        format!("{} s", d.as_secs())
    }
}

fn opt(v: Option<u64>) -> String {
    v.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

/// Draw `app` as of `now`.
pub fn draw(frame: &mut Frame<'_>, app: &App, now: SystemTime) {
    let [top, middle, bottom] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    let h = &app.header;
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let header = Line::from(vec![
        Span::raw("status "),
        Span::styled(status_name(h.status), bold),
        Span::raw(format!(
            "   heartbeat {}   cycle {}   overruns {}   stale {}",
            opt(h.heartbeat),
            opt(h.cycle),
            opt(h.overruns),
            opt(h.stale)
        )),
    ]);
    frame.render_widget(
        Paragraph::new(header).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" taktwerk · {} ", app.endpoint)),
        ),
        top,
    );

    let rows = app.rows.iter().map(|r| {
        TableRow::new(vec![
            r.name.clone(),
            if r.writable { "in" } else { "out" }.to_owned(),
            value::format(&r.value),
            age(now, r.stamp),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(35),
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(9),
        ],
    )
    .header(TableRow::new(vec!["signal", "dir", "value", "age"]).style(bold))
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("> ")
    .block(Block::default().borders(Borders::ALL).title(" signals "));
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, middle, &mut state);

    let first = match (&app.editing, app.rows.get(app.selected)) {
        (Some(text), Some(row)) => format!("{} = {text}_", row.name),
        _ => app.message.clone(),
    };
    let help = if app.editing.is_some() {
        "enter write · esc cancel"
    } else {
        "↑↓ select · enter edit · q quit"
    };
    frame.render_widget(
        Paragraph::new(vec![Line::from(first), Line::from(help)]),
        bottom,
    );
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::app::tests::sample;
    use super::*;

    fn render(app: &App, now: SystemTime) -> String {
        let mut terminal = Terminal::new(TestBackend::new(72, 12)).unwrap();
        terminal.draw(|f| draw(f, app, now)).unwrap();
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        buffer
            .content
            .chunks(width)
            .map(|line| {
                let s: String = line.iter().map(|c| c.symbol()).collect();
                s.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app() -> (App, SystemTime) {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut app = App::new("opc.tcp://127.0.0.1:4840", "taktwerk");
        let mut rows = sample();
        for r in &mut rows {
            r.stamp = Some(now - Duration::from_millis(12));
        }
        rows[5].stamp = Some(now - Duration::from_millis(3400));
        app.update(rows);
        app.message = "connected, 3 signals".into();
        (app, now)
    }

    #[test]
    fn renders_header_table_and_footer() {
        let (app, now) = app();
        let expected = [
            "┌ taktwerk · opc.tcp://127.0.0.1:4840 ─────────────────────────────────┐",
            "│status Running   heartbeat 1234   cycle 1240   overruns 0   stale 6   │",
            "└──────────────────────────────────────────────────────────────────────┘",
            "┌ signals ─────────────────────────────────────────────────────────────┐",
            "│  signal                   dir  value                        age      │",
            "│> plant.k                  in   3                            12 ms    │",
            "│  plant.u                  in   1                            12 ms    │",
            "│  plant.y                  out  0.25                         3.4 s    │",
            "│                                                                      │",
            "└──────────────────────────────────────────────────────────────────────┘",
            "connected, 3 signals",
            "↑↓ select · enter edit · q quit",
        ]
        .join("\n");
        assert_eq!(render(&app, now), expected);
    }

    #[test]
    fn renders_the_edit_line() {
        let (mut app, now) = app();
        app.selected = 1;
        app.editing = Some("2.5".into());
        let text = render(&app, now);
        assert!(
            text.contains("│> plant.u                  in   1"),
            "{text}"
        );
        assert!(
            text.ends_with("plant.u = 2.5_\nenter write · esc cancel"),
            "{text}"
        );
    }

    #[test]
    fn ages() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        assert_eq!(age(now, None), "-");
        assert_eq!(age(now, Some(now - Duration::from_secs(90))), "90 s");
        assert_eq!(age(now, Some(now + Duration::from_secs(1))), "0 ms");
        assert_eq!(status_name(Some(2)), "Faulted");
    }
}
