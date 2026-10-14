use crate::sql::{self, ReadSql, params};

use crate::{Result, model};

const SELECT: &str = "
    SELECT CAST(agreement.id AS TEXT), CAST(agreement.project_id AS TEXT), agreement.name,
           CAST(agreement.hourly_rate_micros AS TEXT), agreement.currency,
           CAST(agreement.minimum_hours AS TEXT), CAST(agreement.maximum_hours AS TEXT),
           agreement.effective_on, CAST(agreement.current AS TEXT),
           CAST(agreement.fee_micros AS TEXT), CAST(agreement.included_hours AS TEXT),
           project.name, client.name, CAST(project.archived_on IS NULL AS TEXT)
    FROM tt_service_agreements agreement
    JOIN tt_projects project ON project.id = agreement.project_id
    JOIN tt_clients client ON client.id = project.client_id
";

impl<S: ReadSql> super::Store<S> {
    pub(crate) fn project_list(&self, all: bool) -> Result<Vec<model::AgreementListing>> {
        sql::query(
            &self.connection,
            &format!(
                "{SELECT} WHERE agreement.current = 1
                 AND (CAST(?1 AS INTEGER) OR project.archived_on IS NULL)
                 ORDER BY project.name, project.id"
            ),
            params![all],
            listing_from_row,
        )
    }

    pub(crate) fn agreement_list(
        &self,
        client: Option<model::ClientId>,
        all: bool,
    ) -> Result<Vec<model::AgreementListing>> {
        let mut filter = "CAST(?1 AS INTEGER) IS NULL";
        if client.is_some() {
            filter = "project.client_id = CAST(?1 AS INTEGER)";
        }
        let mut sql = format!("{SELECT} WHERE {filter}");
        if !all {
            sql.push_str(" AND agreement.current = 1 AND project.archived_on IS NULL");
        }
        sql.push_str(" ORDER BY agreement.project_id, agreement.effective_on, agreement.id");

        sql::query(
            &self.connection,
            &sql,
            params![client.map(model::ClientId::get)],
            listing_from_row,
        )
    }
}

fn listing_from_row(row: &sql::Row) -> Result<model::AgreementListing> {
    Ok(model::AgreementListing {
        agreement: super::agreement_from_row(row)?,
        project_name: row.get(11)?,
        client_name: row.get(12)?,
        project_active: row.get(13)?,
    })
}
