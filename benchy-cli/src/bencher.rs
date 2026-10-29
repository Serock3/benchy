use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use benchy_lib::{Benchmark, BenchmarkStatus, Unit};
use serde::Serialize;

// Bencher's built-in throughput and latency measures have different units. Use custom measures
// to report throughput in Gb/s, time in seconds, and CPU percentages that can exceed 100.
type Metrics = BTreeMap<String, BTreeMap<&'static str, Metric>>;

#[derive(Debug, Serialize, PartialEq)]
struct Metric {
    value: f64,
}

pub fn export(
    result_dir: &Path,
    output: Option<&Path>,
    publication_context: Option<&Path>,
) -> Result<()> {
    let result_dir = result_dir
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", result_dir.display()))?;
    if let Some(output) = output {
        // Do not replace a source document or allow a previous export to become an input.
        let output_parent = output.parent().filter(|path| !path.as_os_str().is_empty());
        let output_parent = output_parent.unwrap_or_else(|| Path::new("."));
        ensure!(
            output_parent.canonicalize()? != result_dir,
            "--output must be outside the result directory"
        );
    }

    let mut paths = fs::read_dir(&result_dir)
        .with_context(|| format!("failed to read {}", result_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();
    ensure!(!paths.is_empty(), "no JSON result files found");

    let benchmarks = paths
        .iter()
        .map(|path| {
            let bytes =
                fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_slice::<Benchmark>(&bytes)
                .with_context(|| format!("invalid benchmark result {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(path) = publication_context {
        let context: crate::github::Publication = serde_json::from_slice(&fs::read(path)?)
            .context("invalid trusted publication context")?;
        context.verify(&benchmarks)?;
    }
    let metrics = convert(&benchmarks)?;
    // Finish validation and serialization before opening the output, so bad input cannot
    // truncate an existing export. Diagnostics stay off stdout, which is valid BMF JSON.
    let mut json = serde_json::to_vec_pretty(&metrics)?;
    json.push(b'\n');
    match output {
        Some(path) => {
            fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
        }
        None => io::stdout()
            .lock()
            .write_all(&json)
            .context("failed to write Bencher metrics to stdout"),
    }
}

fn convert(benchmarks: &[Benchmark]) -> Result<Metrics> {
    let first = benchmarks
        .first()
        .context("no benchmark results provided")?;
    let mut metrics = Metrics::new();
    for benchmark in benchmarks {
        let group = &benchmark.group;
        ensure!(
            benchmark.status == BenchmarkStatus::Success && benchmark.error.is_none(),
            "cannot export unsuccessful benchmark {}/{}",
            group.repository,
            group.name
        );
        ensure!(
            !group.repository.trim().is_empty()
                && !group.name.trim().is_empty()
                && !benchmark.commit.trim().is_empty()
                && !benchmark.branch.trim().is_empty(),
            "repository, benchmark name, commit, and branch must be nonempty"
        );
        ensure!(
            group.repository == first.group.repository
                && benchmark.commit == first.commit
                && benchmark.branch == first.branch
                && benchmark.run_id == first.run_id
                && benchmark.environment.get("GITHUB_RUN_ATTEMPT")
                    == first.environment.get("GITHUB_RUN_ATTEMPT"),
            "result files must belong to the same repository, commit, branch, and workflow attempt"
        );
        ensure!(
            !benchmark.measurements.is_empty(),
            "benchmark {}/{} has no measurements",
            group.repository,
            group.name
        );
        for (id, measurement) in &benchmark.measurements {
            let name = format!("{}/{}/{id}", group.repository, group.name);
            ensure!(
                name.chars().count() <= 1024,
                "Bencher benchmark name exceeds 1024 characters: {name}"
            );
            ensure!(
                measurement.value.is_finite(),
                "measurement {name} has a non-finite value"
            );
            let (measure, value) = convert_measurement(measurement.unit, measurement.value)?;
            ensure!(
                metrics
                    .insert(name.clone(), BTreeMap::from([(measure, Metric { value })]))
                    .is_none(),
                "duplicate benchmark measurement: {name}"
            );
        }
    }
    Ok(metrics)
}

fn convert_measurement(unit: Unit, value: f64) -> Result<(&'static str, f64)> {
    match unit {
        Unit::BitsPerSecond => Ok(("gigabits-per-second", value / 1_000_000_000.0)),
        Unit::Percent => Ok(("percent", value)),
        Unit::Seconds => Ok(("seconds", value)),
        Unit::Count => Ok(("count", value)),
        _ => bail!("unsupported Bencher measurement unit: {unit:?}"),
    }
}

#[cfg(test)]
mod tests {
    use benchy_lib::Measurement;
    use serde_json::json;

    use super::*;

    fn fixture() -> Benchmark {
        serde_json::from_str(include_str!("../tests/fixtures/gotatun.json")).unwrap()
    }

    #[test]
    fn exports_units_without_display_labels_in_identity() {
        let mut benchmark = fixture();
        benchmark.measurements.insert(
            benchy_lib::MetricId::new("latency.control").unwrap(),
            Measurement {
                label: "Control latency".into(),
                unit: Unit::Seconds,
                value: 0.001,
            },
        );
        benchmark.measurements.insert(
            benchy_lib::MetricId::new("packets.received").unwrap(),
            Measurement {
                label: "Received packets".into(),
                unit: Unit::Count,
                value: 42.0,
            },
        );
        let metrics = convert(&[benchmark.clone()]).unwrap();
        assert_eq!(
            serde_json::to_value(&metrics).unwrap(),
            json!({
                "gotatun/gotatun-throughput/cpu.gotatun.up": {"percent": {"value": 282.1}},
                "gotatun/gotatun-throughput/throughput.receiver": {"gigabits-per-second": {"value": 2.73}},
                "gotatun/gotatun-throughput/latency.control": {"seconds": {"value": 0.001}},
                "gotatun/gotatun-throughput/packets.received": {"count": {"value": 42.0}}
            })
        );
        for measurement in benchmark.measurements.values_mut() {
            measurement.label = "New display label".into();
        }
        assert_eq!(metrics, convert(&[benchmark]).unwrap());
    }

    #[test]
    fn throughput_uses_decimal_gigabits_without_rounding_or_mutating_raw_results() {
        let mut benchmark = fixture();
        let bps = 2_692_981_372.578_039;
        benchmark
            .measurements
            .get_mut("throughput.receiver")
            .unwrap()
            .value = bps;
        let metrics = convert(std::slice::from_ref(&benchmark)).unwrap();
        let gbps =
            metrics["gotatun/gotatun-throughput/throughput.receiver"]["gigabits-per-second"].value;
        assert!((gbps - 2.692_981_372_578_039).abs() < 1e-15);
        assert_eq!(benchmark.measurements["throughput.receiver"].value, bps);
        assert_eq!(
            convert_measurement(Unit::BitsPerSecond, 0.0).unwrap().1,
            0.0
        );
    }

    #[test]
    fn rejects_failed_empty_duplicate_and_nonfinite_results() {
        assert!(convert(&[]).is_err());
        let benchmark = fixture();
        assert!(convert(&[benchmark.clone(), benchmark]).is_err());

        let mut failed = fixture();
        failed.status = BenchmarkStatus::Failed;
        assert!(convert(&[failed]).is_err());
        let mut empty = fixture();
        empty.measurements.clear();
        assert!(convert(&[empty]).is_err());
        let mut invalid = fixture();
        invalid.measurements.values_mut().next().unwrap().value = f64::NAN;
        assert!(convert(&[invalid]).is_err());
    }

    #[test]
    fn rejects_mixed_revisions_and_attempts_but_accepts_different_benchmarks() {
        let benchmark = fixture();
        let mut other = benchmark.clone();
        other.group.name = "another-benchmark".into();
        assert_eq!(
            convert(&[benchmark.clone(), other.clone()]).unwrap().len(),
            4
        );
        other.commit = "other-commit".into();
        assert!(convert(&[benchmark.clone(), other.clone()]).is_err());
        other.commit = benchmark.commit.clone();
        other
            .environment
            .insert("GITHUB_RUN_ATTEMPT".into(), "2".into());
        assert!(convert(&[benchmark, other]).is_err());
    }
}
