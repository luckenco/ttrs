CREATE TABLE tt_receipts (
    principal TEXT NOT NULL,
    request_key TEXT NOT NULL,
    envelope TEXT NOT NULL,
    outcome TEXT NOT NULL,
    expires_at_us INTEGER NOT NULL,
    PRIMARY KEY (principal, request_key)
) STRICT;
CREATE INDEX receipts_expiry ON tt_receipts(expires_at_us);
CREATE TABLE tt_receipt_audit (
    source_workspace TEXT NOT NULL,
    source_incarnation TEXT NOT NULL,
    principal TEXT NOT NULL,
    request_key TEXT NOT NULL,
    envelope TEXT NOT NULL,
    outcome TEXT NOT NULL,
    expires_at_us INTEGER NOT NULL,
    PRIMARY KEY (source_workspace, source_incarnation, principal, request_key)
) STRICT;
CREATE TABLE tt_fence (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    state TEXT NOT NULL CHECK (state IN ('active', 'inactive', 'frozen', 'retired')),
    job TEXT,
    receipt_floor_us INTEGER
) STRICT;
INSERT INTO tt_fence(id, state) VALUES (1, 'active');
CREATE TABLE tt_fence_audit (
    source_workspace TEXT NOT NULL,
    source_incarnation TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'inactive', 'frozen', 'retired')),
    job TEXT,
    receipt_floor_us INTEGER,
    PRIMARY KEY (source_workspace, source_incarnation)
) STRICT;

UPDATE tt_metadata SET schema_version = 2 WHERE id = 1;
