// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `SqliteStore` — shared connection + schema bootstrap for every
//! paramodel-persistence trait implemented in this crate.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use paramodel_executor::{ExecutionId, JournalEvent};
use paramodel_persistence::{PersistenceError, PersistenceResult};
use tokio::sync::broadcast;
use tokio_rusqlite::Connection;

/// Simple String-wrapper error so we can fit backend-specific
/// messages into `PersistenceError::Backend(Box<dyn Error>)`.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct BackendMsg(pub String);

/// Shared SQLite-backed store. A single instance implements every
/// trait in `paramodel_persistence`. The underlying
/// [`tokio_rusqlite::Connection`] offloads blocking calls onto a
/// worker thread; callers only ever see `async` futures.
///
/// `SqliteStore::in_memory()` is suitable for tests. Use
/// [`SqliteStore::open`] for a file-backed database.
/// Capacity of the per-execution broadcast channel used by
/// `JournalStore::subscribe`. Tuned for short-lived tests; adopters
/// running long-lived subscribers should dial this up (follow-up
/// slice makes it configurable on `SqliteStore::open`).
const BROADCAST_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct SqliteStore {
    pub(crate) conn: Arc<Connection>,
    /// Per-execution broadcast channels, lazily created by
    /// `publisher_for` and consumed by `JournalStore::subscribe`.
    /// `JournalStore::append` / `append_batch` publish to these
    /// after a successful DB write so live subscribers wake up.
    pub(crate) publishers:
        Arc<Mutex<BTreeMap<ExecutionId, broadcast::Sender<JournalEvent>>>>,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore").finish_non_exhaustive()
    }
}

impl SqliteStore {
    /// Open (or create) a file-backed store at `path`. Runs
    /// migrations on the connection before returning.
    pub async fn open(path: impl AsRef<Path>) -> PersistenceResult<Self> {
        let conn = Connection::open(path)
            .await
            .map_err(rusqlite_err("open"))?;
        let store = Self {
            conn:       Arc::new(conn),
            publishers: Arc::new(Mutex::new(BTreeMap::new())),
        };
        store.migrate().await?;
        Ok(store)
    }

    /// Open a fresh in-memory store. Handy for tests. Each call
    /// returns a distinct, isolated database.
    pub async fn in_memory() -> PersistenceResult<Self> {
        let conn = Connection::open_in_memory()
            .await
            .map_err(rusqlite_err("in_memory"))?;
        let store = Self {
            conn:       Arc::new(conn),
            publishers: Arc::new(Mutex::new(BTreeMap::new())),
        };
        store.migrate().await?;
        Ok(store)
    }

    /// Get or create the broadcast publisher for an execution.
    /// Subscribers created via `subscribe` share this sender.
    pub(crate) fn publisher_for(
        &self,
        exec: ExecutionId,
    ) -> broadcast::Sender<JournalEvent> {
        let mut pubs = self.publishers.lock().expect("poisoned");
        pubs.entry(exec)
            .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
            .clone()
    }

    /// Apply the bundled migration + set per-connection PRAGMAs.
    /// Idempotent — safe to call on an already-initialised database
    /// thanks to `IF NOT EXISTS`.
    async fn migrate(&self) -> PersistenceResult<()> {
        self.conn
            .call(|c| -> Result<(), tokio_rusqlite::rusqlite::Error> {
                c.execute_batch(PRAGMAS)?;
                c.execute_batch(SCHEMA)?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("migrate"))
    }
}

/// Per-connection tuning. Applied once at `migrate()`.
///
/// - `journal_mode=WAL` — readers don't block writers and vice
///   versa; essential for the multi-writer patterns the executor
///   drives (JournalWriter + ExecutionStateManager + external
///   observers all writing to the same DB).
/// - `synchronous=NORMAL` — skip the fsync on every commit, keep
///   the WAL checkpoint fsync. Pragmatic for development; adopters
///   with stricter durability requirements can override by opening
///   with their own Connection.
/// - `foreign_keys=ON` — so the `trial_metrics(trial_id) REFERENCES
///   trial_results(trial_id) ON DELETE CASCADE` edge fires.
pub(crate) const PRAGMAS: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;
";

/// One-shot migration. Every table is created with `IF NOT EXISTS`
/// so the bootstrap is safe to rerun. Schema shape favours JSON
/// blobs for the full domain objects (round-trippable via serde)
/// plus dedicated columns for indexable fields (ids, fingerprints,
/// timestamps, status) that drive filters.
pub(crate) const SCHEMA: &str = r#"
-- Journal events. `sequence` is monotonic *per execution* — the
-- executor's `JournalWriter` allocates it locally, so two different
-- executions may share a sequence number. The composite PK reflects
-- that.
CREATE TABLE IF NOT EXISTS journal_events (
    execution_id  TEXT NOT NULL,
    sequence      INTEGER NOT NULL,
    timestamp     TEXT NOT NULL,
    event_json    TEXT NOT NULL,
    PRIMARY KEY (execution_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_journal_execution
    ON journal_events(execution_id, sequence);

-- Checkpoints.
CREATE TABLE IF NOT EXISTS checkpoints (
    id                TEXT PRIMARY KEY,
    execution_id      TEXT NOT NULL,
    plan_fingerprint  TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    checkpoint_json   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_checkpoints_exec
    ON checkpoints(execution_id, created_at DESC);

-- Execution plans (compiled).
CREATE TABLE IF NOT EXISTS execution_plans (
    id                      TEXT PRIMARY KEY,
    fingerprint             TEXT NOT NULL UNIQUE,
    source_plan_fingerprint TEXT NOT NULL,
    plan_json               TEXT NOT NULL,
    created_at              TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_plans_fp
    ON execution_plans(fingerprint);
CREATE INDEX IF NOT EXISTS idx_plans_source_fp
    ON execution_plans(source_plan_fingerprint);

-- Executions.
CREATE TABLE IF NOT EXISTS executions (
    id               TEXT PRIMARY KEY,
    plan_fingerprint TEXT NOT NULL,
    started_at       TEXT NOT NULL,
    completed_at     TEXT,
    status           TEXT,
    summary_json     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_execs_plan
    ON executions(plan_fingerprint, started_at DESC);

-- Plan + execution-plan metadata (shared table, `scope`
-- discriminates).
CREATE TABLE IF NOT EXISTS plan_metadata (
    scope         TEXT NOT NULL,
    id            TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    labels_json   TEXT NOT NULL DEFAULT '{}',
    tags_json     TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(scope, id)
);

-- Artifacts. Small payloads live inline in `content_blob`; large or
-- external payloads point at `uri` instead.
CREATE TABLE IF NOT EXISTS artifacts (
    id              TEXT PRIMARY KEY,
    execution_id    TEXT NOT NULL,
    trial_id        TEXT,
    element         TEXT NOT NULL,
    name            TEXT NOT NULL,
    content_type    TEXT NOT NULL,
    size_bytes      INTEGER,
    uri             TEXT,
    content_blob    BLOB,
    created_at      TEXT NOT NULL,
    expires_at      TEXT
);
CREATE INDEX IF NOT EXISTS idx_artifacts_exec ON artifacts(execution_id);

-- Trial results.
CREATE TABLE IF NOT EXISTS trial_results (
    trial_id         TEXT PRIMARY KEY,
    execution_id     TEXT,
    plan_fingerprint TEXT,
    status           TEXT NOT NULL,
    attempt          INTEGER NOT NULL,
    started_at       TEXT NOT NULL,
    completed_at     TEXT NOT NULL,
    trial_code       TEXT,
    result_json      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trials_exec ON trial_results(execution_id);
CREATE INDEX IF NOT EXISTS idx_trials_plan ON trial_results(plan_fingerprint);
CREATE INDEX IF NOT EXISTS idx_trials_status ON trial_results(status);

-- Denormalised per-metric rows. One row per (trial, element, param).
-- Lets `ResultFilter::Metric` push down into SQL instead of scanning
-- every trial's JSON blob in Rust.
CREATE TABLE IF NOT EXISTS trial_metrics (
    trial_id   TEXT NOT NULL,
    element    TEXT NOT NULL,
    parameter  TEXT NOT NULL,
    value_kind TEXT NOT NULL,
    value_num  REAL,
    value_text TEXT,
    value_bool INTEGER,
    PRIMARY KEY (trial_id, element, parameter),
    FOREIGN KEY (trial_id) REFERENCES trial_results(trial_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_metrics_coord
    ON trial_metrics(element, parameter);
CREATE INDEX IF NOT EXISTS idx_metrics_num
    ON trial_metrics(element, parameter, value_num);
"#;

/// Map any `Display` error (typically
/// `tokio_rusqlite::Error<rusqlite::Error>` or `rusqlite::Error`)
/// onto a `PersistenceError::Backend`. Generic over the concrete
/// error type so it works for both the outer `.await` result and
/// the inline `rusqlite::Error` returned by `Connection::open`.
pub(crate) fn rusqlite_err<E: std::fmt::Display>(
    ctx: &'static str,
) -> impl Fn(E) -> PersistenceError {
    move |e| PersistenceError::Backend(Box::new(BackendMsg(format!("{ctx}: {e}"))))
}

/// Map a `serde_json::Error` onto a `PersistenceError::Serialization`.
pub(crate) fn json_err(
    ctx: &'static str,
) -> impl Fn(serde_json::Error) -> PersistenceError {
    move |e| PersistenceError::Serialization(Box::new(BackendMsg(format!("{ctx}: {e}"))))
}
