use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::Output,
};

use anyhow::{Context, Result, bail, ensure};
use benchy_lib::{
    Benchmark, BenchmarkGroup, BenchmarkStatus, Measurement, MetricId, SchemaVersion, Uncertainty,
    UncertaintyMethod, Unit, iperf,
};
use chrono::Utc;
use fs2::FileExt;
use tokio::process::Command;

pub mod hotpath;

#[derive(Debug, Clone, Copy)]
pub struct BenchmarkDefinition {
    pub repository: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct MeasurementDefinition {
    pub id: &'static str,
    pub label: &'static str,
    pub unit: Unit,
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
        let commit = env::var("BENCHY_SOURCE_SHA")
            .ok()
            .or_else(|| env::var("GITHUB_SHA").ok())
            .or_else(|| git_value(["rev-parse", "HEAD"]))
            .unwrap_or_else(|| "unknown".to_owned());
        let branch = env::var("BENCHY_SOURCE_BRANCH")
            .ok()
            .or_else(|| env::var("GITHUB_REF_NAME").ok())
            .or_else(|| git_value(["branch", "--show-current"]))
            .unwrap_or_else(|| "unknown".to_owned());
        let commit_message = git_value(["log", "-1", "--pretty=%B"]);

        let mut environment = BTreeMap::new();
        copy_env(&mut environment, "GITHUB_RUN_ATTEMPT");
        copy_env(&mut environment, "GITHUB_RUN_ID");
        copy_env(&mut environment, "GITHUB_RUN_NUMBER");
        copy_env(&mut environment, "BENCHY_SAMPLE_INDEX");
        copy_env(&mut environment, "BENCHY_SAMPLE_COUNT");
        copy_env(&mut environment, "RUNNER_NAME");
        copy_env(&mut environment, "RUNNER_OS");

        Self {
            output: output.into(),
            benchmark: Benchmark {
                schema_version: SchemaVersion,
                group: BenchmarkGroup {
                    repository: definition.repository.to_owned(),
                    name: definition.name.to_owned(),
                },
                commit,
                branch,
                commit_message,
                description: definition.description.to_owned(),
                date: Utc::now(),
                measurements: BTreeMap::new(),
                cases: BTreeMap::new(),
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

    pub fn measurement(&mut self, definition: MeasurementDefinition, value: f64) -> Result<()> {
        self.dynamic_measurement(definition.id, definition.label, definition.unit, value)
    }

    /// Record throughput with an estimate of the mean's uncertainty from one-second intervals.
    pub fn iperf_throughput(
        &mut self,
        definition: MeasurementDefinition,
        value: f64,
        intervals: &iperf::IntervalOutput,
        sender: bool,
    ) -> Result<()> {
        ensure!(
            definition.unit == Unit::BitsPerSecond,
            "iperf throughput must use bits/s"
        );
        let uncertainty = throughput_uncertainty(intervals, sender)?;
        self.record(
            definition.id,
            definition.label,
            definition.unit,
            value,
            uncertainty,
        )
    }

    /// Records a measurement whose identity is only known at runtime, such as one derived from a
    /// profiling report. `id` must satisfy the schema-v1 metric ID grammar.
    pub fn dynamic_measurement(
        &mut self,
        id: &str,
        label: &str,
        unit: Unit,
        value: f64,
    ) -> Result<()> {
        self.record(id, label, unit, value, None)
    }

    /// Record a shared measure under a named sub-benchmark, e.g. one profiled function.
    pub fn case_measurement(
        &mut self,
        case: &str,
        definition: MeasurementDefinition,
        value: f64,
    ) -> Result<()> {
        ensure!(!case.trim().is_empty(), "benchmark case must have a name");
        let (id, measurement) = checked_measurement(
            definition.id,
            definition.label,
            definition.unit,
            value,
            None,
        )?;
        let measurements = self.benchmark.cases.entry(case.to_owned()).or_default();
        ensure!(
            !measurements.contains_key(&id),
            "duplicate case measure {case}/{id}"
        );
        measurements.insert(id, measurement);
        Ok(())
    }

    fn record(
        &mut self,
        id: &str,
        label: &str,
        unit: Unit,
        value: f64,
        uncertainty: Option<Uncertainty>,
    ) -> Result<()> {
        let (id, measurement) = checked_measurement(id, label, unit, value, uncertainty)?;
        ensure!(
            !self.benchmark.measurements.contains_key(&id),
            "metric {id} was recorded more than once"
        );
        self.benchmark.measurements.insert(id, measurement);
        Ok(())
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

/// Estimate s/sqrt(n), assuming independent interval rates and negligible run-level effects.
/// Benchmarks request one-second reports; short final fragments and omitted warmup intervals
/// are excluded. An unavailable side or fewer than two usable intervals yields no bounds.
fn checked_measurement(
    id: &str,
    label: &str,
    unit: Unit,
    value: f64,
    uncertainty: Option<Uncertainty>,
) -> Result<(MetricId, Measurement)> {
    let id = MetricId::new(id)?;
    ensure!(
        !label.trim().is_empty(),
        "metric {id} has an empty display label"
    );
    ensure!(value.is_finite(), "metric {id} has a non-finite value");
    ensure!(
        uncertainty.as_ref().is_none_or(Uncertainty::is_valid),
        "metric {id} has invalid uncertainty"
    );
    Ok((
        id,
        Measurement {
            label: label.to_owned(),
            unit,
            value,
            uncertainty,
        },
    ))
}

pub fn throughput_uncertainty(
    output: &iperf::IntervalOutput,
    sender: bool,
) -> Result<Option<Uncertainty>> {
    let intervals = std::iter::once(output)
        .chain(output.server_output_json.as_deref())
        .find(|output| {
            output
                .intervals
                .iter()
                .any(|interval| interval.sum.sender == Some(sender))
        });
    let Some(output) = intervals else {
        return Ok(None);
    };
    let (mut count, mut mean, mut squared_deviations) = (0_u32, 0.0, 0.0);
    for interval in &output.intervals {
        let sum = &interval.sum;
        if sum.sender != Some(sender) || sum.omitted {
            continue;
        }
        ensure!(
            sum.seconds.is_finite()
                && sum.seconds > 0.0
                && sum.bits_per_second.is_finite()
                && sum.bits_per_second >= 0.0,
            "invalid iperf throughput interval"
        );
        if sum.seconds < 0.9 {
            continue;
        }
        count += 1;
        let delta = sum.bits_per_second - mean;
        mean += delta / f64::from(count);
        squared_deviations += delta * (sum.bits_per_second - mean);
    }
    if count < 2 {
        return Ok(None);
    }
    let standard_error = (squared_deviations / f64::from(count - 1) / f64::from(count)).sqrt();
    let uncertainty = Uncertainty {
        method: UncertaintyMethod::IndependentIntervals,
        sample_count: count,
        standard_error,
    };
    ensure!(
        uncertainty.is_valid(),
        "non-finite iperf interval statistics"
    );
    Ok(Some(uncertainty))
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

pub async fn parse_iperf_udp_output(output: Output) -> Result<iperf::UdpOutput> {
    if !output.status.success() {
        bail!(
            "iperf3 failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("failed to deserialize iperf3 UDP JSON output")
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
    use super::{default_output_path, throughput_uncertainty};
    use benchy_lib::iperf;
    use serde_json::json;

    fn interval_output() -> iperf::IntervalOutput {
        serde_json::from_value(json!({
            "intervals": [
                {"sum": {"seconds": 1.0, "bits_per_second": 1e9, "sender": true}},
                {"sum": {"seconds": 1.0, "bits_per_second": 2e9, "sender": true}},
                {"sum": {"seconds": 1.0, "bits_per_second": 3e9, "sender": true}},
                {"sum": {"seconds": 1.0, "bits_per_second": 99e9, "sender": true, "omitted": true}},
                {"sum": {"seconds": 0.003, "bits_per_second": 99e9, "sender": true}}
            ],
            "server_output_json": {"intervals": [
                {"sum": {"seconds": 1.0, "bits_per_second": 1e8, "sender": false}},
                {"sum": {"seconds": 1.0, "bits_per_second": 2e8, "sender": false}},
                {"sum": {"seconds": 1.0, "bits_per_second": 3e8, "sender": false}}
            ]}
        }))
        .unwrap()
    }

    #[test]
    fn estimates_mean_uncertainty_from_the_correct_endpoint_intervals() {
        let output = interval_output();
        let sender = throughput_uncertainty(&output, true).unwrap().unwrap();
        let receiver = throughput_uncertainty(&output, false).unwrap().unwrap();
        assert_eq!(sender.sample_count, 3);
        assert_eq!(receiver.sample_count, 3);
        assert!((sender.standard_error - 1e9 / 3.0_f64.sqrt()).abs() < 1e-6);
        assert!((receiver.standard_error - 1e8 / 3.0_f64.sqrt()).abs() < 1e-7);
        // Direction flags also work for reverse transfers: the receiver may be local.
        assert_eq!(
            throughput_uncertainty(output.server_output_json.as_ref().unwrap(), false).unwrap(),
            Some(receiver)
        );
    }

    #[test]
    fn missing_or_insufficient_intervals_have_no_uncertainty() {
        assert!(
            throughput_uncertainty(&iperf::IntervalOutput::default(), true)
                .unwrap()
                .is_none()
        );
        let mut output = interval_output();
        output.server_output_json = None;
        assert!(throughput_uncertainty(&output, false).unwrap().is_none());
        output.intervals.truncate(1);
        assert!(throughput_uncertainty(&output, true).unwrap().is_none());
        output.intervals[0].sum.bits_per_second = 0.0;
        output.intervals.push(output.intervals[0].clone());
        assert_eq!(
            throughput_uncertainty(&output, true)
                .unwrap()
                .unwrap()
                .standard_error,
            0.0
        );
    }

    #[test]
    fn rejects_invalid_interval_values() {
        for value in [f64::NAN, f64::INFINITY, -1.0] {
            let mut output = interval_output();
            output.intervals[0].sum.bits_per_second = value;
            assert!(throughput_uncertainty(&output, true).is_err());
        }
        let mut output = interval_output();
        output.intervals[0].sum.seconds = 0.0;
        assert!(throughput_uncertainty(&output, true).is_err());
    }

    #[test]
    fn default_output_uses_benchmark_name() {
        assert!(default_output_path("throughput").ends_with("throughput.json"));
    }
}
