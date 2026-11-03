CREATE TABLE tt_plans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agreement_id INTEGER NOT NULL REFERENCES tt_service_agreements(id) ON DELETE RESTRICT,
    scope TEXT NOT NULL CHECK (length(trim(scope)) > 0),
    started_at_us INTEGER NOT NULL,
    ended_at_us INTEGER NOT NULL,
    CHECK (ended_at_us > started_at_us)
) STRICT;
CREATE INDEX plans_agreement_started ON tt_plans(agreement_id, started_at_us);
UPDATE tt_metadata SET schema_version = 3 WHERE id = 1;
