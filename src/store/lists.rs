use rusqlite::params;

use crate::{Result, model};

const SELECT: &str = "
    SELECT agreement.id, agreement.project_id, agreement.name,
           agreement.hourly_rate_micros, agreement.currency,
           agreement.minimum_hours, agreement.maximum_hours,
           agreement.effective_on, agreement.current,
           project.name, client.name, project.archived_on IS NULL
    FROM service_agreements agreement
    JOIN projects project ON project.id = agreement.project_id
    JOIN clients client ON client.id = project.client_id
";

impl super::Store {
    pub(crate) fn project_list(&self, all: bool) -> Result<Vec<model::AgreementListing>> {
        let mut statement = self.connection.prepare(&format!(
            "{SELECT} WHERE agreement.current = 1 AND (?1 OR project.archived_on IS NULL)
             ORDER BY project.name, project.id"
        ))?;
        let rows = statement.query_map([all], listing_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub(crate) fn agreement_list(
        &self,
        client: Option<model::ClientId>,
        all: bool,
    ) -> Result<Vec<model::AgreementListing>> {
        let mut filter = "?1 IS NULL";
        if client.is_some() {
            filter = "project.client_id = ?1";
        }
        let mut sql = format!("{SELECT} WHERE {filter}");
        if !all {
            sql.push_str(" AND agreement.current = 1 AND project.archived_on IS NULL");
        }
        sql.push_str(" ORDER BY agreement.project_id, agreement.effective_on, agreement.id");
        let mut statement = self.connection.prepare(&sql)?;
        let rows =
            statement.query_map(params![client.map(model::ClientId::get)], listing_from_row)?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }
}

fn listing_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<model::AgreementListing> {
    Ok(model::AgreementListing {
        agreement: super::agreement_from_row(row)?,
        project_name: row.get(9)?,
        client_name: row.get(10)?,
        project_active: row.get(11)?,
    })
}
