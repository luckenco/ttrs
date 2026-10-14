//! Shared rule dispatch; no clocks or transaction lifecycle live here.
use crate::{
    Error, Result, command, model,
    sql::{self, params},
    store, workspace as w,
};
use jiff::{Timestamp, civil::Date, tz::TimeZone};

#[derive(Clone, Debug, serde::Serialize)]
pub enum WorkTime {
    Now,
    At(Timestamp),
}
impl WorkTime {
    fn resolve(self, now: Timestamp) -> Timestamp {
        match self {
            Self::Now => now,
            Self::At(at) => at,
        }
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProjectSelection {
    pub project: model::Project,
    pub agreement: model::ServiceAgreementId,
}
#[derive(Clone, Debug)]
pub enum Operation {
    AddClient {
        name: String,
    },
    AddProject {
        client: model::ClientId,
        name: String,
        zone: String,
        terms: model::NewAgreement,
    },
    Replace {
        previous: model::ServiceAgreementId,
        terms: model::NewAgreement,
    },
    Archive {
        expected: ProjectSelection,
        on: Option<Date>,
    },
    Unarchive {
        expected: ProjectSelection,
    },
    Start {
        agreement: model::ServiceAgreementId,
        scope: String,
        at: WorkTime,
    },
    Stop {
        expected: model::RunningTimer,
        at: WorkTime,
    },
    Switch {
        expected: model::RunningTimer,
        agreement: model::ServiceAgreementId,
        scope: String,
        at: WorkTime,
    },
    Log(command::LogEntry),
    Cancel {
        expected: model::RunningTimer,
    },
    DeleteEntry {
        expected: model::TimeEntry,
    },
}

pub(crate) fn stamp(
    db: &dyn sql::ReadSql,
    now: Timestamp,
    display_zone: Option<String>,
) -> Result<w::Stamp> {
    sql::one(
        db,
        "SELECT workspace, incarnation, CAST(revision AS TEXT), CAST(schema_version AS TEXT), CAST(rules_version AS TEXT) FROM tt_metadata WHERE id = 1",
        &[],
        |r| {
            Ok(w::Stamp {
                authority: w::Authority {
                    workspace: r.get(0)?,
                    incarnation: r.get(1)?,
                },
                revision: r.get(2)?,
                schema: r.get(3)?,
                rules: r.get(4)?,
                calculated_at: Timestamp::from_microsecond(now.as_microsecond())
                    .map_err(|_| Error::InvalidRequest)?,
                display_zone,
            })
        },
    )
}
pub(crate) fn check(
    db: &dyn sql::ReadSql,
    expected: &w::Authority,
    now: Timestamp,
) -> Result<w::Stamp> {
    let stamp = stamp(db, now, None)?;
    if &stamp.authority != expected {
        return Err(Error::AuthorityChanged);
    }
    let frozen: String = sql::one(db, "SELECT state FROM tt_fence WHERE id = 1", &[], |r| {
        r.get(0)
    })?;
    if frozen != "active" {
        return Err(Error::WorkspaceFrozen);
    }
    Ok(stamp)
}
pub(crate) fn bump(tx: &mut dyn sql::WriteSql) -> Result<()> {
    if tx.write("UPDATE tt_metadata SET revision = revision + 1 WHERE id = 1 AND revision < 9223372036854775807 RETURNING CAST(revision AS TEXT)", &[])?.len() != 1 { return Err(sql::invalid("revision exhausted")); }
    Ok(())
}
fn name(value: String, error: Error) -> Result<String> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err(error);
    }
    Ok(value.trim().to_owned())
}
fn zone(value: &str) -> Result<TimeZone> {
    let zone =
        TimeZone::get(value.trim()).map_err(|_| Error::InvalidBillingTimezone(value.into()))?;
    if zone.iana_name().is_none() {
        return Err(Error::InvalidBillingTimezone(value.into()));
    }
    Ok(zone)
}
fn date(date: Date, zone: &TimeZone, now: Timestamp) -> Result<()> {
    if date > now.to_zoned(zone.clone()).date() {
        return Err(Error::EffectiveDateFuture);
    }
    Ok(())
}
fn selection<S: sql::ReadSql>(store: &store::Store<S>, expected: &ProjectSelection) -> Result<()> {
    let id = expected.project.id();
    if store.project(id)?.as_ref() != Some(&expected.project) {
        return Err(Error::ProjectChanged(id));
    }
    if store.current_agreement(id)?.map(|a| a.id()) != Some(expected.agreement) {
        return Err(Error::AgreementChanged(expected.agreement));
    }
    Ok(())
}
pub(crate) fn apply(
    tx: &mut dyn sql::WriteSql,
    authority: &w::Authority,
    operation: Operation,
    now: Timestamp,
) -> Result<w::Committed> {
    check(tx, authority, now)?;
    let mut store = store::Store {
        connection: &mut *tx,
    };
    let value = match operation {
        Operation::AddClient { name: n } => {
            w::MutationValue::Client(store.add_client(name(n, Error::EmptyClientName)?)?)
        }
        Operation::AddProject {
            client,
            name: n,
            zone: z,
            terms,
        } => {
            name(terms.clone().into_parts().0, Error::EmptyAgreementName)?;
            let z = zone(&z)?;
            date(terms.effective_on(), &z, now)?;
            let (p, a) = store.add_project(client, name(n, Error::EmptyProjectName)?, z, terms)?;
            w::MutationValue::Project(p, a)
        }
        Operation::Replace { previous, terms } => {
            name(terms.clone().into_parts().0, Error::EmptyAgreementName)?;
            let a = store
                .agreement(previous)?
                .ok_or(Error::AgreementNotFound(previous))?;
            let p = store
                .project(a.project())?
                .ok_or(Error::ProjectNotFound(a.project()))?;
            date(terms.effective_on(), p.billing_timezone(), now)?;
            w::MutationValue::Agreement(store.add_agreement(previous, terms)?)
        }
        Operation::Archive { expected, on } => {
            selection(&store, &expected)?;
            let on = on.unwrap_or_else(|| {
                now.to_zoned(expected.project.billing_timezone().clone())
                    .date()
            });
            date(on, expected.project.billing_timezone(), now)?;
            w::MutationValue::Archived(store.archive_project(expected.project.id(), on)?)
        }
        Operation::Unarchive { expected } => {
            selection(&store, &expected)?;
            w::MutationValue::Archived(store.unarchive_project(expected.project.id())?)
        }
        Operation::Start {
            agreement,
            scope,
            at,
        } => {
            let (a, s, t) = command::Start::new(agreement, scope, at.resolve(now))?.into_parts();
            w::MutationValue::Timer(store.start(a, s, t)?)
        }
        Operation::Stop { expected, at } => {
            let (_, at) = command::End::new(expected.id(), at.resolve(now)).into_parts();
            w::MutationValue::Entry(store.end_if_current(&expected, at)?)
        }
        Operation::Switch {
            expected,
            agreement,
            scope,
            at,
        } => {
            let (a, s, t) = command::Start::new(agreement, scope, at.resolve(now))?.into_parts();
            let (e, t) = store.switch(&expected, a, s, t)?;
            w::MutationValue::Switched(e, t)
        }
        Operation::Log(log) => {
            let (a, s, start, end) = log.into_parts();
            w::MutationValue::Entry(store.log(a, s, start, end)?)
        }
        Operation::Cancel { expected } => {
            store.cancel_if_current(&expected)?;
            w::MutationValue::Removed
        }
        Operation::DeleteEntry { expected } => {
            store.delete_entry_if_current(&expected)?;
            w::MutationValue::Removed
        }
    };
    bump(tx)?;
    Ok(w::Committed {
        value,
        stamp: stamp(tx, now, None)?,
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    authority: w::Authority,
    revision: String,
    agreement: String,
    limit: u16,
    start: String,
    id: String,
}
pub(crate) fn read(
    db: &dyn sql::ReadSql,
    query: w::Query,
    now: Timestamp,
) -> Result<w::Projection> {
    let mut stamp = stamp(db, now, None)?;
    let store = store::Store { connection: db };
    let value = match query {
        w::Query::Lookup(reference) => w::ProjectionValue::Item(match reference {
            w::Reference::Client(id) => {
                w::Item::Client(store.client(id)?.ok_or(Error::ClientNotFound(id))?)
            }
            w::Reference::Project(id) => {
                w::Item::Project(store.project(id)?.ok_or(Error::ProjectNotFound(id))?)
            }
            w::Reference::Agreement(id) => {
                w::Item::Agreement(store.agreement(id)?.ok_or(Error::AgreementNotFound(id))?)
            }
            w::Reference::Timer(id) => w::Item::Timer(store.timer(id)?),
            w::Reference::Entry(id) => w::Item::Entry(store.entry(id)?),
        }),
        w::Query::Overview { zone } => {
            stamp.display_zone = Some(crate::protocol::display_zone(&zone)?);
            w::ProjectionValue::Overview(Box::new(w::Overview {
                dashboard: store.dashboard(now, &zone)?,
                clients: store.clients()?,
                projects: store.projects()?,
                agreements: store.agreements()?,
                status: store.status(now)?,
            }))
        }
        w::Query::RecentScopes { project, limit } => {
            if limit == 0 || limit > 100 {
                return Err(Error::InvalidRequest);
            }
            w::ProjectionValue::Scopes(store.recent_scopes(project, usize::from(limit))?)
        }
        w::Query::Entries {
            agreement,
            limit,
            cursor,
        } => {
            if limit == 0 || limit > 200 {
                return Err(Error::InvalidRequest);
            }
            if store.agreement(agreement)?.is_none() {
                return Err(Error::AgreementNotFound(agreement));
            }
            let mut start = i64::MAX;
            let mut id = i64::MAX;
            if let Some(cursor) = cursor {
                let c: Cursor = serde_json::from_str(&cursor).map_err(|_| Error::InvalidRequest)?;
                if c.authority != stamp.authority {
                    return Err(Error::AuthorityChanged);
                }
                if c.revision != stamp.revision.to_string() {
                    return Err(Error::RevisionChanged);
                }
                if c.agreement != agreement.to_string() || c.limit != limit {
                    return Err(Error::InvalidRequest);
                }
                start = sql::integer(&c.start).map_err(|_| Error::InvalidRequest)?;
                id = sql::integer(&c.id).map_err(|_| Error::InvalidRequest)?;
            }
            let mut entries = sql::query(
                db,
                "SELECT CAST(id AS TEXT), scope, CAST(started_at_us AS TEXT), CAST(ended_at_us AS TEXT) FROM tt_time_entries WHERE agreement_id = CAST(?1 AS INTEGER) AND (started_at_us < CAST(?2 AS INTEGER) OR (started_at_us = CAST(?2 AS INTEGER) AND id < CAST(?3 AS INTEGER))) ORDER BY started_at_us DESC, id DESC LIMIT CAST(?4 AS INTEGER)",
                params![agreement.get(), start, id, i64::from(limit) + 1],
                |r| {
                    Ok(model::TimeEntry::new(
                        model::EntryId::from_raw(r.get(0)?),
                        agreement,
                        r.get(1)?,
                        Timestamp::from_microsecond(r.get(2)?)
                            .map_err(|_| Error::InvalidRequest)?,
                        Timestamp::from_microsecond(r.get(3)?)
                            .map_err(|_| Error::InvalidRequest)?,
                    ))
                },
            )?;
            let cursor = if entries.len() > usize::from(limit) {
                entries.pop();
                let last = entries.last().unwrap();
                Some(
                    serde_json::to_string(&Cursor {
                        authority: stamp.authority.clone(),
                        revision: stamp.revision.to_string(),
                        agreement: agreement.to_string(),
                        limit,
                        start: last.started_at().as_microsecond().to_string(),
                        id: last.id().to_string(),
                    })
                    .map_err(|_| Error::InvalidRequest)?,
                )
            } else {
                None
            };
            w::ProjectionValue::Entries { entries, cursor }
        }
    };
    Ok(w::Projection { stamp, value })
}
