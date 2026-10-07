//! The monitor's state and key handling, free of terminal and network I/O.

use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use opcua::types::Variant;

use super::value;

/// One signal as last read.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Signal name.
    pub name: String,
    /// Inputs and tunables are writable; outputs and system signals are not.
    pub writable: bool,
    /// Last value read; `Empty` before the first read.
    pub value: Variant,
    /// When the engine wrote the value (the node's source timestamp).
    pub stamp: Option<SystemTime>,
}

/// The engine's own signals, read from the system rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Header {
    /// `status`.
    pub status: Option<i64>,
    /// `heartbeat`.
    pub heartbeat: Option<u64>,
    /// `cycle`.
    pub cycle: Option<u64>,
    /// `overruns`.
    pub overruns: Option<u64>,
    /// `stale`.
    pub stale: Option<u64>,
}

/// What the event loop must do after a key.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Nothing beyond redrawing.
    None,
    /// Write `value` to signal `name`.
    Write {
        /// Signal name.
        name: String,
        /// New value.
        value: Variant,
    },
    /// Leave the monitor.
    Quit,
}

/// The monitor's state.
#[derive(Debug, Clone, Default)]
pub struct App {
    /// The server being watched.
    pub endpoint: String,
    /// The engine's `system_prefix`.
    pub prefix: String,
    /// Engine signals.
    pub header: Header,
    /// Every non-system signal, sorted by name.
    pub rows: Vec<Row>,
    /// Index of the selected row.
    pub selected: usize,
    /// Text being edited for the selected row.
    pub editing: Option<String>,
    /// Last connection state or write outcome.
    pub message: String,
}

fn as_u64(v: &Variant) -> Option<u64> {
    match v {
        Variant::UInt64(n) => Some(*n),
        _ => None,
    }
}

impl App {
    /// A monitor of `endpoint` for an engine with `prefix`.
    pub fn new(endpoint: &str, prefix: &str) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            prefix: prefix.to_owned(),
            message: "connecting…".to_owned(),
            ..Self::default()
        }
    }

    /// Take a fresh read of every signal; system signals go to the header.
    pub fn update(&mut self, signals: Vec<Row>) {
        let selected = self.rows.get(self.selected).map(|r| r.name.clone());
        let system = format!("{}.", self.prefix);
        let mut header = Header::default();
        let mut rows = Vec::with_capacity(signals.len());
        for row in signals {
            match row.name.strip_prefix(&system) {
                Some("status") => {
                    header.status = match row.value {
                        Variant::Int32(n) => Some(i64::from(n)),
                        _ => None,
                    };
                }
                Some("heartbeat") => header.heartbeat = as_u64(&row.value),
                Some("cycle") => header.cycle = as_u64(&row.value),
                Some("overruns") => header.overruns = as_u64(&row.value),
                Some("stale") => header.stale = as_u64(&row.value),
                Some(_) => {}
                None => rows.push(row),
            }
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        self.header = header;
        self.rows = rows;
        self.selected = selected
            .and_then(|name| self.rows.iter().position(|r| r.name == name))
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }

    /// Handle one key press.
    pub fn key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if let Some(text) = &mut self.editing {
            match key.code {
                KeyCode::Esc => self.editing = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                KeyCode::Enter => return self.commit(),
                _ => {}
            }
            return Action::None;
        }
        let last = self.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(last),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = last,
            KeyCode::Enter => match self.rows.get(self.selected) {
                Some(row) if row.writable => {
                    self.editing = Some(value::format(&row.value));
                }
                Some(row) => self.message = format!("{} is read-only", row.name),
                None => {}
            },
            _ => {}
        }
        Action::None
    }

    /// Parse the edited text against the selected row's value.
    fn commit(&mut self) -> Action {
        let Some(text) = self.editing.take() else {
            return Action::None;
        };
        let Some(row) = self.rows.get(self.selected) else {
            return Action::None;
        };
        match value::parse(&row.value, &text) {
            Ok(value) => {
                self.message = format!("writing {} …", row.name);
                Action::Write {
                    name: row.name.clone(),
                    value,
                }
            }
            Err(e) => {
                self.message = format!("{}: {e}", row.name);
                Action::None
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
pub(crate) mod tests {
    use crossterm::event::KeyEventKind;

    use super::*;

    pub(crate) fn row(name: &str, writable: bool, value: Variant) -> Row {
        Row {
            name: name.into(),
            writable,
            value,
            stamp: None,
        }
    }

    pub(crate) fn sample() -> Vec<Row> {
        vec![
            row("taktwerk.status", false, Variant::Int32(1)),
            row("taktwerk.heartbeat", false, Variant::UInt64(1234)),
            row("taktwerk.cycle", false, Variant::UInt64(1240)),
            row("taktwerk.overruns", false, Variant::UInt64(0)),
            row("taktwerk.stale", false, Variant::UInt64(6)),
            row("plant.y", false, Variant::Double(0.25)),
            row("plant.u", true, Variant::Double(1.0)),
            row("plant.k", true, Variant::Int32(3)),
        ]
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Press,
        ))
    }

    #[test]
    fn system_signals_fill_the_header() {
        let mut app = App::new("opc.tcp://x:1", "taktwerk");
        app.update(sample());
        assert_eq!(app.header.status, Some(1));
        assert_eq!(app.header.heartbeat, Some(1234));
        assert_eq!(app.header.stale, Some(6));
        let names: Vec<_> = app.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["plant.k", "plant.u", "plant.y"]);
    }

    #[test]
    fn edits_a_writable_signal() {
        let mut app = App::new("opc.tcp://x:1", "taktwerk");
        app.update(sample());
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.editing.as_deref(), Some("1"));
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Char('4'));
        press(&mut app, KeyCode::Char('.'));
        press(&mut app, KeyCode::Char('5'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Write {
                name: "plant.u".into(),
                value: Variant::Double(4.5)
            }
        );
        assert_eq!(app.editing, None);
    }

    #[test]
    fn refuses_read_only_and_bad_text() {
        let mut app = App::new("opc.tcp://x:1", "taktwerk");
        app.update(sample());
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.editing, None);
        assert_eq!(app.message, "plant.y is read-only");
        press(&mut app, KeyCode::Home);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(app.message.starts_with("plant.k: "), "{}", app.message);
        assert_eq!(press(&mut app, KeyCode::Char('q')), Action::Quit);
    }

    #[test]
    fn selection_follows_the_name() {
        let mut app = App::new("opc.tcp://x:1", "taktwerk");
        app.update(sample());
        press(&mut app, KeyCode::Down);
        let mut more = sample();
        more.push(row("a.first", false, Variant::Double(0.0)));
        app.update(more);
        assert_eq!(app.rows[app.selected].name, "plant.u");
    }
}
