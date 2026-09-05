use std::{
    io::{self, IsTerminal},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use jiff::tz::TimeZone;
use tt::Tracker;

use super::{Error, app, io_error};

mod input;
mod search;
mod state;
#[cfg(test)]
mod tests;
mod view;

fn duration(us: i128) -> String {
    let seconds = us.max(0) / 1_000_000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

struct PasteMode;

impl Drop for PasteMode {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(), event::DisableBracketedPaste);
    }
}

pub(super) fn run(tracker: &mut Tracker, zone: &TimeZone) -> Result<(), Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::usage(
            "interactive mode requires a terminal; use an explicit command",
        ));
    }
    let now = app::now()?;
    let snapshot = tracker
        .dashboard(now, zone)
        .map_err(|e| Error::runtime(e.to_string()))?;
    let mut app = state::App::new(snapshot);
    let quit = Arc::new(AtomicBool::new(false));
    let signals = [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ]
    .into_iter()
    .map(|signal| signal_hook::flag::register(signal, quit.clone()))
    .collect::<io::Result<Vec<_>>>()
    .map_err(io_error)?;
    // Ratatui installs a panic hook. Restore on ordinary errors too, including
    // partial initialization failure, before the CLI prints diagnostics.
    let result = ratatui::try_init()
        .map_err(io_error)
        .and_then(|mut terminal| {
            let _paste = PasteMode;
            crossterm::execute!(io::stdout(), event::EnableBracketedPaste).map_err(io_error)?;
            let mut refresh = Instant::now() + Duration::from_secs(1);
            let mut now = now;
            loop {
                if quit.load(Ordering::Relaxed) {
                    return Ok(());
                }
                app.expire(Instant::now());
                terminal
                    .draw(|frame| view::draw(frame, &mut app, now, zone))
                    .map_err(io_error)?;
                let mut wake = refresh;
                if let Some(state::Message::Success { until, .. }) = &app.message {
                    wake = wake.min(*until);
                }
                if event::poll(wake.saturating_duration_since(Instant::now())).map_err(io_error)? {
                    let event = event::read().map_err(io_error)?;
                    let size = terminal.size().map_err(io_error)?;
                    if size.width < view::MIN_WIDTH || size.height < view::MIN_HEIGHT {
                        if let Event::Key(key) = event
                            && (key.code == KeyCode::Char('q')
                                || (key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL)))
                        {
                            return Ok(());
                        }
                        continue;
                    }
                    now = super::app::now()?;
                    match event {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            if matches!(app.key(key, tracker, now, zone), Ok(true)) {
                                return Ok(());
                            }
                        }
                        Event::Paste(text) => app.paste(&text),
                        _ => {}
                    }
                }
                if Instant::now() >= refresh {
                    now = super::app::now()?;
                    app.refresh(tracker, now, zone);
                    refresh = Instant::now() + Duration::from_secs(1);
                }
            }
        });
    ratatui::restore();
    for signal in signals {
        signal_hook::low_level::unregister(signal);
    }
    result
}
