use jiff::{Timestamp, tz::TimeZone};
use tt::{Tracker, model};

use super::{
    catalog::Reference,
    choose::Choose,
    form::{Form, Kind},
    state::{App, core_error},
    workspace::Overlay,
};
use crate::cli::Error;

pub(super) fn entry_label(entry: &model::TimeEntry, zone: &TimeZone) -> String {
    format!(
        "id:{} · {} · {} — {}",
        entry.id(),
        entry.scope(),
        entry
            .started_at()
            .to_zoned(zone.clone())
            .strftime("%F %T%.f %:z %Z"),
        entry
            .ended_at()
            .to_zoned(zone.clone())
            .strftime("%F %T%.f %:z %Z")
    )
}

pub(super) fn entry_detail(entry: &model::TimeEntry, zone: &TimeZone) -> String {
    format!(
        "{}\nid:{} · agreement:{}\nFrom: {}\nTo: {}\nDuration: {}",
        entry.scope(),
        entry.id(),
        entry.agreement(),
        entry
            .started_at()
            .to_zoned(zone.clone())
            .strftime("%F %T%.f %:z %Z"),
        entry
            .ended_at()
            .to_zoned(zone.clone())
            .strftime("%F %T%.f %:z %Z"),
        super::duration(
            i128::from(entry.ended_at().as_microsecond())
                - i128::from(entry.started_at().as_microsecond())
        )
    )
}

impl App {
    pub(super) fn cancel_review(&mut self, now: Timestamp, zone: &TimeZone) -> Result<(), Error> {
        let timer = self
            .selection
            .selected()
            .and_then(|i| self.snapshot.timers.get(i))
            .ok_or_else(|| Error::usage("Select a running timer first."))?
            .timer
            .clone();
        self.recovery_form(Kind::Cancel(timer), now, zone)
    }

    pub(super) fn browse_entries(
        &mut self,
        tracker: &Tracker,
        zone: &TimeZone,
    ) -> Result<(), Error> {
        let agreement = match self.workspace.reference() {
            Some(Reference::Agreement(id)) => id,
            Some(Reference::Project(id)) => self.workspace.catalogs.current(id)?.id(),
            _ => return Err(Error::usage("Select a project or agreement first.")),
        };
        let entries = tracker.entries(agreement).map_err(core_error)?;
        self.workspace.overlay = Some(Overlay::Entries(Choose::new(
            "Completed entries",
            entries.into_iter().map(|entry| {
                let label = entry_label(&entry, zone);
                (entry, label)
            }),
        )));
        Ok(())
    }

    pub(super) fn recovery_form(
        &mut self,
        kind: Kind,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<(), Error> {
        if self.refresh_error.is_some() {
            return Err(Error::runtime(
                "Database unavailable. Reopen after refresh succeeds.",
            ));
        }
        let mut form = Form::new(kind, &self.workspace.catalogs, None, now, zone)?;
        form.review = Some(form.prepare(&self.workspace.catalogs, now, zone)?);
        self.error = None;
        self.workspace.overlay = Some(Overlay::Form(Box::new(form)));
        Ok(())
    }
}
