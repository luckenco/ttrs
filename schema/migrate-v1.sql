ALTER TABLE clients RENAME TO tt_clients;
ALTER TABLE projects RENAME TO tt_projects;
ALTER TABLE service_agreements RENAME TO tt_service_agreements;
ALTER TABLE running_timers RENAME TO tt_running_timers;
ALTER TABLE time_entries RENAME TO tt_time_entries;
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
