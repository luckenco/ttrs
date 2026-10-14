CREATE TABLE tt_clients (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0)
    ) STRICT;

    CREATE TABLE tt_projects (
        id               INTEGER PRIMARY KEY,
        client_id        INTEGER NOT NULL REFERENCES tt_clients(id) ON DELETE RESTRICT,
        name             TEXT NOT NULL CHECK (length(trim(name)) > 0),
        billing_timezone TEXT NOT NULL CHECK (length(trim(billing_timezone)) > 0),
        archived_on      TEXT,
        UNIQUE (client_id, name)
    ) STRICT;

    CREATE INDEX projects_name ON tt_projects(name, id);

    CREATE TABLE tt_service_agreements (
        id                  INTEGER PRIMARY KEY,
        project_id          INTEGER NOT NULL REFERENCES tt_projects(id) ON DELETE RESTRICT,
        name                TEXT NOT NULL CHECK (length(trim(name)) > 0),
        hourly_rate_micros   INTEGER CHECK (hourly_rate_micros >= 0),
        fee_micros          INTEGER CHECK (fee_micros >= 0),
        included_hours      INTEGER CHECK (included_hours BETWEEN 0 AND 4294967295),
        currency            TEXT NOT NULL CHECK (currency GLOB '[A-Z][A-Z][A-Z]'),
        minimum_hours       INTEGER CHECK (minimum_hours >= 0),
        maximum_hours       INTEGER CHECK (maximum_hours >= 0),
        effective_on        TEXT NOT NULL,
        current             INTEGER NOT NULL CHECK (current IN (0, 1)),
        UNIQUE (project_id, name),
        CHECK (
            (fee_micros IS NULL AND included_hours IS NULL AND hourly_rate_micros IS NOT NULL)
            OR (fee_micros IS NOT NULL AND included_hours IS NOT NULL AND minimum_hours IS NULL)
        ),
        CHECK (
            minimum_hours IS NULL OR maximum_hours IS NULL
            OR minimum_hours <= maximum_hours
        )
    ) STRICT;

    CREATE UNIQUE INDEX service_agreements_one_current_per_project
        ON tt_service_agreements(project_id) WHERE current = 1;

    CREATE INDEX service_agreements_project_effective
        ON tt_service_agreements(project_id, effective_on);

    CREATE TABLE tt_running_timers (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        agreement_id  INTEGER NOT NULL REFERENCES tt_service_agreements(id) ON DELETE RESTRICT,
        scope         TEXT NOT NULL CHECK (length(trim(scope)) > 0),
        started_at_us INTEGER NOT NULL
    ) STRICT;

    CREATE INDEX running_timers_agreement_started
        ON tt_running_timers(agreement_id, started_at_us DESC);

    CREATE TABLE tt_time_entries (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        agreement_id  INTEGER NOT NULL REFERENCES tt_service_agreements(id) ON DELETE RESTRICT,
        scope         TEXT NOT NULL CHECK (length(trim(scope)) > 0),
        started_at_us INTEGER NOT NULL,
        ended_at_us   INTEGER NOT NULL,
        CHECK (ended_at_us > started_at_us)
    ) STRICT;

    CREATE INDEX time_entries_agreement_started
        ON tt_time_entries(agreement_id, started_at_us DESC);

    CREATE INDEX time_entries_agreement_ended
        ON tt_time_entries(agreement_id, ended_at_us, started_at_us);

    CREATE INDEX time_entries_ended ON tt_time_entries(ended_at_us);
CREATE TABLE tt_metadata (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    schema_version INTEGER NOT NULL,
    rules_version INTEGER NOT NULL,
    workspace TEXT NOT NULL,
    incarnation TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0)
) STRICT;
CREATE TABLE clients (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL
) STRICT;
CREATE TRIGGER clients_reject_insert BEFORE INSERT ON clients BEGIN SELECT RAISE(ABORT, 'legacy writes forbidden'); END;
CREATE TRIGGER clients_reject_update BEFORE UPDATE ON clients BEGIN SELECT RAISE(ABORT, 'legacy writes forbidden'); END;
CREATE TRIGGER clients_reject_delete BEFORE DELETE ON clients BEGIN SELECT RAISE(ABORT, 'legacy writes forbidden'); END;
