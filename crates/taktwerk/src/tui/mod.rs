//! `taktwerk tui`: a terminal monitor of a running engine, as an OPC UA client of its server.
//!
//! Shows status, heartbeat and counters, and every signal with its value and age; the selected
//! input or tunable can be edited and written back.

mod app;
mod client;
mod value;
mod view;

use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::{mpsc, watch};

use app::{Action, App};

/// Redraw at least this often.
const FRAME: Duration = Duration::from_millis(100);

/// Monitor the engine serving `endpoint` until the user quits.
///
/// # Errors
/// The terminal or the I/O runtime cannot be set up.
pub fn run(endpoint: &str, namespace: &str, prefix: &str) -> anyhow::Result<ExitCode> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .context("cannot start the I/O runtime")?;
    let (rows_tx, mut rows) = watch::channel(Vec::new());
    let (msg_tx, messages) = std::sync::mpsc::channel();
    let (writes, writes_rx) = mpsc::unbounded_channel();
    let feed = client::Feed {
        rows: rows_tx,
        messages: msg_tx,
    };
    rt.spawn(client::serve(
        endpoint.to_owned(),
        namespace.to_owned(),
        feed,
        writes_rx,
    ));

    let mut app = App::new(endpoint, prefix);
    let mut terminal = ratatui::try_init().context("cannot set up the terminal")?;
    let result = (|| -> anyhow::Result<()> {
        loop {
            while let Ok(m) = messages.try_recv() {
                app.message = m;
            }
            if rows.has_changed().unwrap_or(false) {
                let fresh = rows.borrow_and_update().clone();
                app.update(fresh);
            }
            terminal.draw(|f| view::draw(f, &app, SystemTime::now()))?;
            if !event::poll(FRAME)? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.key(key) {
                Action::None => {}
                Action::Quit => return Ok(()),
                Action::Write { name, value } => {
                    let _ = writes.send((name, value));
                }
            }
        }
    })();
    ratatui::try_restore().context("cannot restore the terminal")?;
    drop(writes);
    rt.shutdown_timeout(Duration::from_secs(1));
    result.map(|()| ExitCode::SUCCESS)
}
