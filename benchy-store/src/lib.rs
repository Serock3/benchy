use std::{fs, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use benchy_lib::{Benchmark, RESULT_SCHEMA_VERSION};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

const DATABASE_SCHEMA_VERSION: u32 = 1;

pub struct Store {
    connection: Connection,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngestReport {
    pub inserted: usize,
    pub updated: usize,
}

impl IngestReport {
    pub const fn total(self) -> usize {
        self.inserted + self.updated
    }
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open SQLite database {}", path.display()))?;
        Self::from_connection(connection)
    }

    pub fn ingest(&mut self, benchmarks: &[Benchmark]) -> Result<IngestReport> {
        for benchmark in benchmarks {
            validate(benchmark)?;
        }

        let transaction = self.connection.transaction()?;
        let mut report = IngestReport::default();
        for benchmark in benchmarks {
            if ingest_one(&transaction, benchmark)? {
                report.updated += 1;
            } else {
                report.inserted += 1;
            }
        }
        transaction.commit()?;
        Ok(report)
    }

    fn from_connection(connection: Connection) -> Result<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;",
        )?;

        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => migrate_to_v1(&connection)?,
            DATABASE_SCHEMA_VERSION => {}
            newer if newer > DATABASE_SCHEMA_VERSION => bail!(
                "database schema version {newer} is newer than supported version {DATABASE_SCHEMA_VERSION}"
            ),
            older => bail!("unsupported database schema version {older}"),
        }

        Ok(Self { connection })
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }
}

fn migrate_to_v1(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
         CREATE TABLE runs (
             id INTEGER PRIMARY KEY,
             result_schema_version INTEGER NOT NULL,
             repository TEXT NOT NULL,
             benchmark TEXT NOT NULL,
             commit_hash TEXT NOT NULL,
             branch TEXT NOT NULL,
             commit_message TEXT,
             description TEXT NOT NULL,
             started_at TEXT NOT NULL,
             workflow_run_id TEXT,
             workflow_run_attempt TEXT NOT NULL DEFAULT '',
             workflow_run_number TEXT,
             status TEXT NOT NULL CHECK (status IN ('success', 'failed')),
             error TEXT,
             raw_json TEXT NOT NULL
         );
         CREATE UNIQUE INDEX runs_ci_identity
             ON runs(repository, benchmark, workflow_run_id, workflow_run_attempt)
             WHERE workflow_run_id IS NOT NULL;
         CREATE INDEX runs_series_time
             ON runs(repository, benchmark, started_at);
         CREATE TABLE measurements (
             run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
             metric_id TEXT NOT NULL,
             label TEXT NOT NULL,
             unit TEXT NOT NULL CHECK (
                 unit IN ('bits_per_second', 'percent', 'seconds', 'count')
             ),
             value REAL NOT NULL,
             PRIMARY KEY (run_id, metric_id)
         );
         CREATE INDEX measurements_metric
             ON measurements(metric_id, run_id);
         CREATE TABLE parameters (
             run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
             key TEXT NOT NULL,
             value TEXT NOT NULL,
             PRIMARY KEY (run_id, key)
         );
         CREATE TABLE environment (
             run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
             key TEXT NOT NULL,
             value TEXT NOT NULL,
             PRIMARY KEY (run_id, key)
         );
         PRAGMA user_version = 1;
         COMMIT;",
        )
        .context("failed to create database schema v1")
}

fn validate(benchmark: &Benchmark) -> Result<()> {
    if benchmark.group.repository.trim().is_empty() {
        bail!("benchmark repository must not be empty");
    }
    if benchmark.group.name.trim().is_empty() {
        bail!("benchmark name must not be empty");
    }
    for (id, measurement) in &benchmark.measurements {
        if measurement.label.trim().is_empty() {
            bail!("measurement {id} has an empty label");
        }
        if !measurement.value.is_finite() {
            bail!("measurement {id} has a non-finite value");
        }
    }
    Ok(())
}

/// Returns true when an existing CI run was updated.
fn ingest_one(transaction: &Transaction<'_>, benchmark: &Benchmark) -> Result<bool> {
    let attempt = benchmark
        .environment
        .get("GITHUB_RUN_ATTEMPT")
        .map(String::as_str)
        .unwrap_or("");
    let run_number = benchmark
        .environment
        .get("GITHUB_RUN_NUMBER")
        .map(String::as_str);

    let existing_id = match &benchmark.run_id {
        Some(run_id) => transaction
            .query_row(
                "SELECT id FROM runs
                 WHERE repository = ?1 AND benchmark = ?2
                   AND workflow_run_id = ?3 AND workflow_run_attempt = ?4",
                params![
                    &benchmark.group.repository,
                    &benchmark.group.name,
                    run_id,
                    attempt
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?,
        None => None,
    };

    let raw_json = serde_json::to_string(benchmark)?;
    let started_at = benchmark.date.to_rfc3339();
    let run_id = if let Some(id) = existing_id {
        transaction.execute(
            "UPDATE runs SET
                 result_schema_version = ?1, commit_hash = ?2, branch = ?3,
                 commit_message = ?4, description = ?5, started_at = ?6,
                 workflow_run_number = ?7, status = ?8, error = ?9, raw_json = ?10
             WHERE id = ?11",
            params![
                RESULT_SCHEMA_VERSION,
                &benchmark.commit,
                &benchmark.branch,
                &benchmark.commit_message,
                &benchmark.description,
                started_at,
                run_number,
                benchmark.status.as_str(),
                &benchmark.error,
                raw_json,
                id,
            ],
        )?;
        transaction.execute("DELETE FROM measurements WHERE run_id = ?1", [id])?;
        transaction.execute("DELETE FROM parameters WHERE run_id = ?1", [id])?;
        transaction.execute("DELETE FROM environment WHERE run_id = ?1", [id])?;
        id
    } else {
        transaction.execute(
            "INSERT INTO runs (
                 result_schema_version, repository, benchmark, commit_hash, branch,
                 commit_message, description, started_at, workflow_run_id,
                 workflow_run_attempt, workflow_run_number, status, error, raw_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                RESULT_SCHEMA_VERSION,
                &benchmark.group.repository,
                &benchmark.group.name,
                &benchmark.commit,
                &benchmark.branch,
                &benchmark.commit_message,
                &benchmark.description,
                started_at,
                &benchmark.run_id,
                attempt,
                run_number,
                benchmark.status.as_str(),
                &benchmark.error,
                raw_json,
            ],
        )?;
        transaction.last_insert_rowid()
    };

    for (id, measurement) in &benchmark.measurements {
        transaction.execute(
            "INSERT INTO measurements (run_id, metric_id, label, unit, value)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                run_id,
                id.as_str(),
                &measurement.label,
                measurement.unit.as_str(),
                measurement.value,
            ],
        )?;
    }
    for (key, value) in &benchmark.parameters {
        transaction.execute(
            "INSERT INTO parameters (run_id, key, value) VALUES (?1, ?2, ?3)",
            params![run_id, key, value],
        )?;
    }
    for (key, value) in &benchmark.environment {
        transaction.execute(
            "INSERT INTO environment (run_id, key, value) VALUES (?1, ?2, ?3)",
            params![run_id, key, value],
        )?;
    }

    Ok(existing_id.is_some())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use benchy_lib::{
        Benchmark, BenchmarkGroup, BenchmarkStatus, Measurement, MetricId, SchemaVersion, Unit,
    };
    use rusqlite::Connection;

    use super::Store;

    fn result(value: f64) -> Benchmark {
        Benchmark {
            schema_version: SchemaVersion,
            group: BenchmarkGroup {
                repository: "gotatun".to_owned(),
                name: "gotatun-throughput".to_owned(),
            },
            commit: "abc123".to_owned(),
            branch: "main".to_owned(),
            commit_message: Some("Benchmark storage".to_owned()),
            description: "GotaTun throughput".to_owned(),
            date: "2026-09-25T12:00:00Z".parse().unwrap(),
            measurements: BTreeMap::from([(
                MetricId::new("throughput.receiver").unwrap(),
                Measurement {
                    label: "Receiver throughput".to_owned(),
                    unit: Unit::BitsPerSecond,
                    value,
                },
            )]),
            run_id: Some("123456".to_owned()),
            status: BenchmarkStatus::Success,
            error: None,
            parameters: BTreeMap::from([("duration_seconds".to_owned(), "30".to_owned())]),
            environment: BTreeMap::from([
                ("GITHUB_RUN_ATTEMPT".to_owned(), "1".to_owned()),
                ("GITHUB_RUN_NUMBER".to_owned(), "42".to_owned()),
                ("RUNNER_NAME".to_owned(), "benchy-alice".to_owned()),
            ]),
        }
    }

    #[test]
    fn creates_schema_and_normalizes_results() {
        let mut store = Store::open_in_memory().unwrap();
        let report = store.ingest(&[result(2_500_000_000.0)]).unwrap();
        assert_eq!(report.inserted, 1);
        assert_eq!(report.updated, 0);

        let version: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let row: (String, String, f64) = store
            .connection
            .query_row(
                "SELECT runs.repository, measurements.metric_id, measurements.value
                 FROM runs JOIN measurements ON measurements.run_id = runs.id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "gotatun".to_owned(),
                "throughput.receiver".to_owned(),
                2_500_000_000.0
            )
        );
    }

    #[test]
    fn retrying_the_same_workflow_attempt_updates_the_run() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.ingest(&[result(1.0)]).unwrap().inserted, 1);
        let report = store.ingest(&[result(2.0)]).unwrap();
        assert_eq!(report.inserted, 0);
        assert_eq!(report.updated, 1);

        let counts: (i64, i64) = store
            .connection
            .query_row(
                "SELECT (SELECT count(*) FROM runs), (SELECT count(*) FROM measurements)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1));
        let value: f64 = store
            .connection
            .query_row("SELECT value FROM measurements", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, 2.0);
    }

    #[test]
    fn rejects_newer_database_schema() {
        let connection = Connection::open_in_memory().unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        let error = Store::from_connection(connection).err().unwrap();
        assert!(error.to_string().contains("newer than supported"));
    }
}
