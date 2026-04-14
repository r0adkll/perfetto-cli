CREATE TABLE IF NOT EXISTS devices (
    serial     TEXT PRIMARY KEY,
    nickname   TEXT,
    model      TEXT,
    last_seen  TEXT
);

CREATE TABLE IF NOT EXISTS sessions (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    name                 TEXT NOT NULL,
    package_name         TEXT NOT NULL,
    device_serial        TEXT,
    config_json          TEXT NOT NULL,
    folder_path          TEXT NOT NULL,
    created_at           TEXT NOT NULL,
    notes                TEXT,
    is_imported          INTEGER NOT NULL DEFAULT 0,
    benchmark_json_path  TEXT,
    import_source_dir    TEXT,
    FOREIGN KEY (device_serial) REFERENCES devices(serial)
);

CREATE TABLE IF NOT EXISTS traces (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   INTEGER NOT NULL,
    file_path    TEXT NOT NULL,
    label        TEXT,
    duration_ms  INTEGER,
    size_bytes   INTEGER,
    captured_at  TEXT NOT NULL,
    remote_url   TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS tags (
    name TEXT PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS trace_tags (
    trace_id  INTEGER NOT NULL,
    tag_name  TEXT NOT NULL,
    PRIMARY KEY (trace_id, tag_name),
    FOREIGN KEY (trace_id) REFERENCES traces(id) ON DELETE CASCADE,
    FOREIGN KEY (tag_name) REFERENCES tags(name) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS configs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE,
    config_json TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS command_sets (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT NOT NULL UNIQUE,
    commands_json TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Per-app custom PerfettoSQL queries saved from the Analysis SQL REPL.
-- Scoped by `package_name` so users get an app-specific dashboard that
-- persists across sessions targeting the same app.
CREATE TABLE IF NOT EXISTS saved_queries (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    package_name  TEXT NOT NULL,
    name          TEXT NOT NULL,
    sql           TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    UNIQUE(package_name, name)
);

CREATE INDEX IF NOT EXISTS idx_traces_session ON traces(session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_created ON sessions(created_at);
CREATE INDEX IF NOT EXISTS idx_saved_queries_package ON saved_queries(package_name);
