//! Old interaction tests drain the real effect/completion boundary synchronously.
//! Delay/order tests below deliberately do not drain it.
use super::{effects, state::App, workspace::Overlay};
use crate::cli::{Error, backend::Backend};
use crossterm::event::KeyEvent;
use jiff::{Timestamp, tz::TimeZone};

impl App {
    pub(super) fn drain(
        &mut self,
        backend: &mut Backend,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<(), Error> {
        let mut error = None;
        while let Some(effect) = self.pending.take() {
            let completion = effects::execute(backend, effect, Ok(now));
            match &completion {
                effects::Completion::Refresh { result: Err(e), .. }
                | effects::Completion::Read { result: Err(e), .. }
                | effects::Completion::Applied { result: Err(e), .. } => {
                    error = Some(Error::runtime(e.to_string()))
                }
                _ => {}
            }
            self.complete(completion, zone);
        }
        if let Some(error) = error {
            return Err(error);
        }
        Ok(())
    }
    pub(super) fn key_sync(
        &mut self,
        key: KeyEvent,
        backend: &mut Backend,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<bool, Error> {
        if self.pending.context.is_none() {
            self.refresh_sync(backend, now, zone);
        }
        let quit = self.key(key, now, zone)?;
        self.drain(backend, now, zone)?;
        if let Some(Overlay::Form(form)) = &self.workspace.overlay
            && let Some(error) = &form.error
        {
            return Err(Error::runtime(error.clone()));
        }
        Ok(quit)
    }
    pub(super) fn refresh_sync(&mut self, backend: &mut Backend, now: Timestamp, zone: &TimeZone) {
        self.request_refresh(zone);
        let _ = self.drain(backend, now, zone);
    }
}
