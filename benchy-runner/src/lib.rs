use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::Output,
};

use anyhow::{Context, Result, bail};
use benchy_lib::{Benchmark, BenchmarkGroup, BenchmarkStatus, Value, iperf};
use chrono::Utc;
use fs2::FileExt;
use tokio::process::Command;

#[derive(Debug, Clone, Copy)]
pub struct BenchmarkDefinition {
    pub repository: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

pub struct Recorder {
    output: PathBuf,
    benchmark: Benchmark,
}

pub struct MachineLock {
    _file: File,
}

impl MachineLock {
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("failed to open machine lock {}", path.display()))?;
        file.try_lock_exclusive()
            .with_context(|| format!("benchmark machine is already locked ({})", path.display()))?;
        Ok(Self { _file: file })
    }
}

impl Recorder {
    pub async fn new(definition: BenchmarkDefinition, output: impl Into<PathBuf>) -> Self {
        let commit = env::var("GITHUB_SHA")
            .ok()
            .or_else(|| git_value(["rev-parse", "HEAD"]))
            .unwrap_or_else(|| "unknown".to_owned());
        let branch = env::var("GITHUB_REF_NAME")
            .ok()
            .or_else(|| git_value(["branch", "--show-current"]))
            .unwrap_or_else(|| "unknown".to_owned());
        let commit_message = git_value(["log", "-1", "--pretty=%B"]);

        let mut environment = BTreeMap::new();
        copy_env(&mut environment, "GITHUB_RUN_ATTEMPT");
        copy_env(&mut environment, "GITHUB_RUN_ID");
        copy_env(&mut environment, "GITHUB_RUN_NUMBER");
        copy_env(&mut environment, "RUNNER_NAME");
        copy_env(&mut environment, "RUNNER_OS");

        Self {
            output: output.into(),
            benchmark: Benchmark {
                group: BenchmarkGroup {
                    repository: definition.repository.to_owned(),
                    name: definition.name.to_owned(),
                },
                commit,
                branch,
                commit_message,
                description: definition.description.to_owned(),
                date: Utc::now(),
                values: BTreeMap::new(),
                run_id: env::var("GITHUB_RUN_ID").ok(),
                status: BenchmarkStatus::Success,
                error: None,
                parameters: BTreeMap::new(),
                environment,
            },
        }
    }

    pub fn parameter(&mut self, key: impl Into<String>, value: impl ToString) {
        self.benchmark
            .parameters
            .insert(key.into(), value.to_string());
    }

    pub fn environment(&mut self, key: impl Into<String>, value: impl ToString) {
        self.benchmark
            .environment
            .insert(key.into(), value.to_string());
    }

    pub fn value(&mut self, name: impl Into<String>, value: Value) {
        self.benchmark.values.insert(name.into(), value);
    }

    pub async fn success(self) -> Result<()> {
        self.write().await
    }

    pub async fn failure(mut self, error: &anyhow::Error) -> Result<()> {
        self.benchmark.status = BenchmarkStatus::Failed;
        self.benchmark.error = Some(format!("{error:#}"));
        self.write().await
    }

    async fn write(self) -> Result<()> {
        if let Some(parent) = self.output.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(&self.benchmark)?;
        tokio::fs::write(&self.output, json)
            .await
            .with_context(|| format!("failed to write {}", self.output.display()))
    }
}

pub async fn checked_output<I, S>(program: impl AsRef<OsStr>, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program = program.as_ref();
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to execute {}", program.to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "{} failed with {}: {}",
            program.to_string_lossy(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

pub async fn parse_iperf_output(output: Output) -> Result<iperf::Output> {
    if !output.status.success() {
        bail!(
            "iperf3 failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("failed to deserialize iperf3 JSON output")
}

pub fn default_output_path(name: &str) -> PathBuf {
    env::var_os("BENCHY_RESULT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("results").to_owned())
        .join(format!("{name}.json"))
}

fn git_value<const N: usize>(args: [&str; N]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn copy_env(target: &mut BTreeMap<String, String>, key: &str) {
    if let Ok(value) = env::var(key) {
        target.insert(key.to_owned(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::default_output_path;

    #[test]
    fn default_output_uses_benchmark_name() {
        assert!(default_output_path("throughput").ends_with("throughput.json"));
    }
}
