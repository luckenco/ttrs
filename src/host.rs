//! Private synchronous adapter contract. Host owns transactions, authentication and clocks.
//! An Err from attempt MUST roll back, not commit ordinary callback data.
pub use crate::protocol::{
    Envelope, RecordedOutcome, Resolution, Session, attempt, decode_committed, decode_operation,
    encode_operation, error_code, is_domain, record_rejection, request_status,
};
pub use crate::sql::{Cell, ReadSql, Row, WriteSql};
use crate::{Result, Timestamp, operation, portable, workspace};

pub const SCHEMA_VERSION: u32 = crate::schema::VERSION;
pub const RULES_VERSION: u32 = crate::schema::RULES;
pub const SCHEMA: &str = crate::schema::SCHEMA;
pub const MIGRATE_V2: &str = crate::schema::MIGRATE_V2;

/// DDL is a separate capability used only with trusted shared schema constants.
/// Implementations must not permit transaction-control SQL to escape host ownership.
pub trait SchemaSql: WriteSql {
    fn batch(&mut self, sql: &str) -> Result<()>;
}
pub fn create(
    tx: &mut dyn SchemaSql,
    authority: &workspace::Authority,
    active: bool,
) -> Result<()> {
    let count: i64 = crate::sql::one(
        tx,
        "SELECT CAST(count(*) AS TEXT) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
        &[],
        |r| r.get(0),
    )?;
    if count != 0 {
        return Err(crate::Error::DatabaseSchemaOutdated);
    }
    tx.batch(SCHEMA)?;
    initialize(tx, authority, active)
}
/// Verify version and exact shared definitions before returning any service handle.
pub fn verify_schema(db: &dyn ReadSql) -> Result<()> {
    let stamp = operation::stamp(db, Timestamp::UNIX_EPOCH, None)?;
    if stamp.schema != SCHEMA_VERSION
        || stamp.rules != RULES_VERSION
        || !crate::schema::matches(db, SCHEMA)?
    {
        return Err(crate::Error::DatabaseSchemaOutdated);
    }
    Ok(())
}
/// Host must durably retain the independently captured verified backup BEFORE this transaction.
/// Compare the complete legacy snapshot and exact schema under the writer reservation.
pub fn migrate_v1(tx: &mut dyn SchemaSql, backup: &portable::Verified) -> Result<()> {
    if backup.manifest().format != 1
        || backup.manifest().schema != 1
        || !crate::schema::matches(tx, crate::schema::V1)?
    {
        return Err(crate::Error::DatabaseSchemaOutdated);
    }
    let now = backup
        .manifest()
        .captured_at
        .parse()
        .map_err(|_| crate::Error::InvalidRequest)?;
    if portable::capture(tx, now)?.to_bytes()? != backup.to_bytes()? {
        return Err(crate::Error::InvalidBackup(
            "migration backup does not match source",
        ));
    }
    tx.batch(MIGRATE_V2)?;
    verify_schema(tx)?;
    if portable::capture(tx, now)?.manifest().data_digest != backup.manifest().data_digest {
        return Err(crate::Error::InvalidBackup(
            "migration changed domain data or revision",
        ));
    }
    Ok(())
}

/// After host executes shared schema SQL, initialize fresh identity in the same transaction.
pub fn initialize(
    tx: &mut dyn WriteSql,
    authority: &workspace::Authority,
    active: bool,
) -> Result<()> {
    crate::protocol::identity(&authority.workspace)?;
    crate::protocol::identity(&authority.incarnation)?;
    tx.write("INSERT INTO tt_metadata(id,schema_version,rules_version,workspace,incarnation,revision) VALUES (1,CAST(?1 AS INTEGER),CAST(?2 AS INTEGER),?3,?4,0) RETURNING CAST(id AS TEXT)", crate::sql::params![SCHEMA_VERSION,RULES_VERSION,authority.workspace,authority.incarnation])?;
    if !active {
        tx.write(
            "UPDATE tt_fence SET state = 'inactive' WHERE id = 1 RETURNING state",
            &[],
        )?;
    }
    Ok(())
}
pub fn session(db: &dyn ReadSql, principal: &str, now: Timestamp) -> Result<Session> {
    Ok(Session {
        principal: principal.into(),
        stamp: operation::stamp(db, now, None)?,
    })
}
pub fn read(
    db: &dyn ReadSql,
    query: workspace::Query,
    now: Timestamp,
) -> Result<workspace::Projection> {
    operation::read(db, query, now)
}
pub fn apply(
    tx: &mut dyn WriteSql,
    authority: &workspace::Authority,
    op: operation::Operation,
    now: Timestamp,
) -> Result<workspace::Committed> {
    operation::apply(tx, authority, op, now)
}
pub fn capture(db: &dyn ReadSql, now: Timestamp) -> Result<portable::Verified> {
    portable::capture(db, now)
}
pub fn import(tx: &mut dyn WriteSql, snapshot: &portable::Verified) -> Result<()> {
    let state: String = crate::sql::one(tx, "SELECT state FROM tt_fence WHERE id = 1", &[], |r| {
        r.get(0)
    })?;
    if state != "inactive" {
        return Err(crate::Error::WorkspaceFrozen);
    }
    portable::import(tx, snapshot)
}
/// JSON query DTO conversion. Display zones are bundled and never host-clock derived.
pub fn decode_query(bytes: &[u8]) -> Result<workspace::Query> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    enum Dto {
        Lookup {
            kind: String,
            id: String,
        },
        Overview {
            zone: String,
        },
        Entries {
            agreement: String,
            limit: u16,
            cursor: Option<String>,
        },
        RecentScopes {
            project: String,
            limit: u16,
        },
    }
    let dto: Dto = serde_json::from_slice(bytes).map_err(|_| crate::Error::InvalidRequest)?;
    match dto {
        Dto::Lookup { kind, id } => {
            let id = crate::sql::integer(&id).map_err(|_| crate::Error::InvalidRequest)?;
            let reference = match kind.as_str() {
                "Client" => workspace::Reference::Client(
                    id.try_into().map_err(|_| crate::Error::InvalidRequest)?,
                ),
                "Project" => workspace::Reference::Project(
                    id.try_into().map_err(|_| crate::Error::InvalidRequest)?,
                ),
                "Agreement" => workspace::Reference::Agreement(
                    id.try_into().map_err(|_| crate::Error::InvalidRequest)?,
                ),
                "Timer" => workspace::Reference::Timer(
                    id.try_into().map_err(|_| crate::Error::InvalidRequest)?,
                ),
                "Entry" => workspace::Reference::Entry(
                    id.try_into().map_err(|_| crate::Error::InvalidRequest)?,
                ),
                _ => return Err(crate::Error::InvalidRequest),
            };
            Ok(workspace::Query::Lookup(reference))
        }
        Dto::Overview { zone } => Ok(workspace::Query::Overview {
            zone: crate::protocol::parse_display_zone(&zone)?,
        }),
        Dto::Entries {
            agreement,
            limit,
            cursor,
        } => Ok(workspace::Query::Entries {
            agreement: crate::model::ServiceAgreementId::try_from(
                crate::sql::integer(&agreement).map_err(|_| crate::Error::InvalidRequest)?,
            )
            .map_err(|_| crate::Error::InvalidRequest)?,
            limit,
            cursor,
        }),
        Dto::RecentScopes { project, limit } => Ok(workspace::Query::RecentScopes {
            project: crate::model::ProjectId::try_from(
                crate::sql::integer(&project).map_err(|_| crate::Error::InvalidRequest)?,
            )
            .map_err(|_| crate::Error::InvalidRequest)?,
            limit,
        }),
    }
}
