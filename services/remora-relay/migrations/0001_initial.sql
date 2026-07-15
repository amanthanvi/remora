BEGIN;

CREATE TABLE IF NOT EXISTS relay_schema (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    version INTEGER NOT NULL,
    updated_at_ms BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS installations (
    id TEXT PRIMARY KEY,
    write_capability_hash BYTEA NOT NULL CHECK (octet_length(write_capability_hash) = 32),
    read_capability_hash BYTEA NOT NULL CHECK (octet_length(read_capability_hash) = 32),
    manage_capability_hash BYTEA NOT NULL CHECK (octet_length(manage_capability_hash) = 32),
    next_sequence BIGINT NOT NULL CHECK (next_sequence >= 1),
    replay_floor BIGINT NOT NULL CHECK (replay_floor >= 1),
    acknowledged_through BIGINT NOT NULL DEFAULT 0 CHECK (acknowledged_through >= 0),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    tombstoned_at_ms BIGINT,
    CONSTRAINT installations_ack_below_high_watermark
        CHECK (acknowledged_through < next_sequence)
);

ALTER TABLE installations
    ADD COLUMN IF NOT EXISTS acknowledged_through BIGINT NOT NULL DEFAULT 0
    CHECK (acknowledged_through >= 0);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'installations'::regclass
          AND conname = 'installations_ack_below_high_watermark'
    ) THEN
        ALTER TABLE installations ADD CONSTRAINT installations_ack_below_high_watermark
            CHECK (acknowledged_through < next_sequence);
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS installation_receipts (
    idempotency_key_hash BYTEA PRIMARY KEY CHECK (octet_length(idempotency_key_hash) = 32),
    request_digest BYTEA NOT NULL CHECK (octet_length(request_digest) = 32),
    installation_id TEXT NOT NULL,
    response_nonce BYTEA CHECK (response_nonce IS NULL OR octet_length(response_nonce) = 24),
    response_ciphertext BYTEA,
    response_expires_at_ms BIGINT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    CHECK ((response_nonce IS NULL) = (response_ciphertext IS NULL))
);

CREATE INDEX IF NOT EXISTS installation_receipts_installation_idx
    ON installation_receipts(installation_id);

CREATE TABLE IF NOT EXISTS events (
    installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    sequence BIGINT NOT NULL CHECK (sequence >= 1),
    event_id TEXT NOT NULL,
    request_digest BYTEA NOT NULL CHECK (octet_length(request_digest) = 32),
    event_class TEXT NOT NULL CHECK (event_class IN (
        'state_changed', 'activity_changed', 'connection_changed', 'security_changed'
    )),
    expires_at_ms BIGINT NOT NULL,
    ciphertext BYTEA NOT NULL,
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (installation_id, sequence),
    UNIQUE (installation_id, event_id)
);

CREATE INDEX IF NOT EXISTS events_expiry_idx ON events(expires_at_ms);

CREATE TABLE IF NOT EXISTS event_receipts (
    installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    event_id TEXT NOT NULL,
    request_digest BYTEA NOT NULL CHECK (octet_length(request_digest) = 32),
    sequence BIGINT NOT NULL CHECK (sequence >= 1),
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (installation_id, event_id)
);

INSERT INTO event_receipts (installation_id, event_id, request_digest, sequence, created_at_ms)
SELECT installation_id, event_id, request_digest, sequence, created_at_ms FROM events
ON CONFLICT (installation_id, event_id) DO NOTHING;

CREATE TABLE IF NOT EXISTS snapshots (
    installation_id TEXT PRIMARY KEY REFERENCES installations(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision >= 1),
    through_sequence BIGINT NOT NULL CHECK (through_sequence >= 1),
    snapshot_digest BYTEA NOT NULL CHECK (octet_length(snapshot_digest) = 32),
    expires_at_ms BIGINT NOT NULL,
    ciphertext BYTEA NOT NULL,
    updated_at_ms BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS device_registrations (
    id TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK (provider IN ('apns', 'fcm')),
    environment TEXT NOT NULL CHECK (environment IN ('sandbox', 'production')),
    token_hash BYTEA NOT NULL CHECK (octet_length(token_hash) = 32),
    token_nonce BYTEA NOT NULL CHECK (octet_length(token_nonce) = 24),
    token_ciphertext BYTEA NOT NULL,
    generation BIGINT NOT NULL CHECK (generation >= 1),
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    tombstoned_at_ms BIGINT,
    UNIQUE (installation_id, provider, environment),
    CHECK (provider <> 'fcm' OR environment = 'production')
);

CREATE TABLE IF NOT EXISTS push_outbox (
    id TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    registration_id TEXT NOT NULL REFERENCES device_registrations(id) ON DELETE CASCADE,
    event_id TEXT NOT NULL,
    cursor BIGINT NOT NULL CHECK (cursor >= 1),
    event_class TEXT NOT NULL CHECK (event_class IN (
        'state_changed', 'activity_changed', 'connection_changed', 'security_changed'
    )),
    expires_at_ms BIGINT NOT NULL,
    generation BIGINT NOT NULL CHECK (generation >= 1),
    registration_generation BIGINT NOT NULL CHECK (registration_generation >= 1),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at_ms BIGINT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'leased', 'delivered', 'suppressed', 'expired', 'dead_letter', 'tombstoned'
    )),
    lease_id TEXT,
    lease_until_ms BIGINT,
    last_error_class TEXT,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    UNIQUE (registration_id, event_class)
);

CREATE INDEX IF NOT EXISTS outbox_due_idx
    ON push_outbox(state, next_attempt_at_ms, expires_at_ms);

CREATE INDEX IF NOT EXISTS outbox_registration_idx
    ON push_outbox(registration_id);

ALTER TABLE push_outbox ADD COLUMN IF NOT EXISTS lease_id TEXT;

INSERT INTO relay_schema (singleton, version, updated_at_ms)
VALUES (TRUE, 4, 0)
ON CONFLICT (singleton) DO UPDATE SET
    version = EXCLUDED.version,
    updated_at_ms = EXCLUDED.updated_at_ms
WHERE relay_schema.version < EXCLUDED.version;

COMMIT;
