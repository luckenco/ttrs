use std::cmp::Reverse;

use jiff::{Timestamp, tz::TimeZone};
use tt::model;

use super::{choose::Choose, recovery};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Entry {
    Completed(model::TimeEntry),
    Planned(model::Plan),
}

impl Entry {
    fn started_at(&self) -> Timestamp {
        match self {
            Self::Completed(entry) => entry.started_at(),
            Self::Planned(plan) => plan.started_at(),
        }
    }

    pub fn label(&self, zone: &TimeZone) -> String {
        match self {
            Self::Completed(entry) => format!("Completed · {}", recovery::entry_label(entry, zone)),
            Self::Planned(plan) => format!("Planned · {}", recovery::plan_label(plan, zone)),
        }
    }

    pub fn detail(&self, zone: &TimeZone) -> String {
        match self {
            Self::Completed(entry) => format!("Completed\n{}", recovery::entry_detail(entry, zone)),
            Self::Planned(plan) => format!(
                "Planned\n{}\nagreement:{}\nDuration: {}\nReserved until explicitly completed or cancelled.",
                recovery::plan_label(plan, zone),
                plan.agreement(),
                super::duration(
                    i128::from(plan.ended_at().as_microsecond())
                        - i128::from(plan.started_at().as_microsecond())
                )
            ),
        }
    }
}

#[derive(Clone, Copy)]
enum Filter {
    All,
    Completed,
    Planned,
}

pub(super) struct Browser {
    entries: Vec<Entry>,
    filter: Filter,
    pub picker: Choose<Entry>,
}

impl Browser {
    pub fn new(mut entries: Vec<Entry>, zone: &TimeZone) -> Self {
        entries.sort_by_key(|entry| Reverse(entry.started_at()));
        let picker = Choose::new(
            "Entries · All · Alt-f filter",
            entries
                .iter()
                .map(|entry| (entry.clone(), entry.label(zone))),
        );
        Self {
            entries,
            filter: Filter::All,
            picker,
        }
    }

    pub fn cycle_filter(&mut self, zone: &TimeZone) {
        self.filter = match self.filter {
            Filter::All => Filter::Completed,
            Filter::Completed => Filter::Planned,
            Filter::Planned => Filter::All,
        };
        let title = match self.filter {
            Filter::All => "Entries · All · Alt-f filter",
            Filter::Completed => "Entries · Completed · Alt-f filter",
            Filter::Planned => "Entries · Planned · Alt-f filter",
        };
        let query = self.picker.input.as_str().to_owned();
        self.picker = Choose::new(
            title,
            self.entries
                .iter()
                .filter(|entry| {
                    matches!(
                        (self.filter, entry),
                        (Filter::All, _)
                            | (Filter::Completed, Entry::Completed(_))
                            | (Filter::Planned, Entry::Planned(_))
                    )
                })
                .map(|entry| (entry.clone(), entry.label(zone))),
        );
        self.picker.paste(&query);
    }
}
