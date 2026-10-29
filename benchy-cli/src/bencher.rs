use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use benchy_lib::{Benchmark, BenchmarkStatus, Measurement, Unit};
use serde::Serialize;

// Bencher's built-in throughput and latency measures have different units. Use custom measures
// to report throughput in Gb/s, time in seconds, and CPU percentages that can exceed 100.
type Metrics = BTreeMap<String, BTreeMap<String, Metric>>;

#[derive(Debug, Serialize, PartialEq)]
struct Measure {
    name: String,
    units: &'static str,
}

#[derive(Debug, PartialEq)]
struct Export {
    metrics: Metrics,
    measures: BTreeMap<String, Measure>,
}

#[derive(Debug, Serialize, PartialEq)]
struct Metric {
    value: f64,
}

pub fn export(
    result_dir: &Path,
    output: Option<&Path>,
    measures_output: Option<&Path>,
    publication_context: Option<&Path>,
) -> Result<()> {
    let result_dir = result_dir
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", result_dir.display()))?;
    let mut destinations = Vec::new();
    for output in [output, measures_output].into_iter().flatten() {
        // Do not replace a source document or allow a previous export to become an input.
        let output_parent = output.parent().filter(|path| !path.as_os_str().is_empty());
        let output_parent = output_parent.unwrap_or_else(|| Path::new("."));
        let parent = output_parent.canonicalize()?;
        ensure!(
            parent != result_dir,
            "--output must be outside the result directory"
        );
        let destination = parent.join(output.file_name().context("output must name a file")?);
        ensure!(
            !destinations.contains(&destination),
            "metrics and measure definitions must use different output files"
        );
        destinations.push(destination);
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
    let export = convert(&benchmarks)?;
    // Finish validation and serialization before opening the output, so bad input cannot
    // truncate an existing export. Diagnostics stay off stdout, which is valid BMF JSON.
    let mut json = serde_json::to_vec_pretty(&export.metrics)?;
    json.push(b'\n');
    if let Some(path) = measures_output {
        let mut definitions = serde_json::to_vec_pretty(&export.measures)?;
        definitions.push(b'\n');
        fs::write(path, definitions)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
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

fn convert(benchmarks: &[Benchmark]) -> Result<Export> {
    let first = benchmarks
        .first()
        .context("no benchmark results provided")?;
    let mut metrics = Metrics::new();
    let mut measures = BTreeMap::new();
    let mut measure_ids = BTreeMap::new();
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
        let name = &group.name;
        ensure!(
            name.chars().count() <= 1024,
            "Bencher benchmark name exceeds 1024 characters: {name}"
        );
        let mut values = BTreeMap::new();
        for (id, measurement) in &benchmark.measurements {
            ensure!(
                measurement.value.is_finite(),
                "measurement {name}/{id} has a non-finite value"
            );
            let slug = id.as_str().replace(['.', '_'], "-");
            ensure!(
                slug.len() <= 64,
                "Bencher measure slug exceeds 64 characters: {slug}"
            );
            if let Some(previous) = measure_ids.insert(slug.clone(), id) {
                ensure!(
                    previous == id,
                    "metric IDs collide on Bencher measure {slug}"
                );
            }
            let (definition, value) = convert_measurement(id.as_str(), measurement)?;
            if let Some(previous) = measures.insert(slug.clone(), definition) {
                ensure!(
                    previous == measures[&slug],
                    "conflicting names or units for Bencher measure {slug}"
                );
            }
            values.insert(slug, Metric { value });
        }
        ensure!(
            metrics.insert(name.clone(), values).is_none(),
            "duplicate benchmark: {name}"
        );
    }
    Ok(Export { metrics, measures })
}

fn convert_measurement(id: &str, measurement: &Measurement) -> Result<(Measure, f64)> {
    let (units, suffix, value) = match measurement.unit {
        Unit::BitsPerSecond => ("Gb/s", "Gb/s", measurement.value / 1_000_000_000.0),
        Unit::Percent if id.starts_with("cpu.") => {
            ("% of one logical CPU", "logical CPU %", measurement.value)
        }
        Unit::Percent => ("%", "%", measurement.value),
        Unit::Seconds => ("s", "s", measurement.value),
        Unit::Count => ("count", "count", measurement.value),
        unit => bail!("unsupported Bencher measurement unit: {unit:?}"),
    };
    let label = measurement.label.trim();
    ensure!(!label.is_empty(), "measurement {id} has an empty label");
    let label = label
        .strip_suffix(" (% of one logical CPU)")
        .unwrap_or(label);
    let name = format!("{label} ({suffix})");
    ensure!(
        name.chars().count() <= 64,
        "Bencher measure name exceeds 64 characters: {name}"
    );
    Ok((Measure { name, units }, value))
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
    fn groups_measurements_under_one_benchmark_with_stable_measure_ids() {
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
        let export = convert(&[benchmark.clone()]).unwrap();
        assert_eq!(
            serde_json::to_value(&export.metrics).unwrap(),
            json!({
                "gotatun-throughput": {
                    "cpu-gotatun-up": {"value": 282.1},
                    "throughput-receiver": {"value": 2.73},
                    "latency-control": {"value": 0.001},
                    "packets-received": {"value": 42.0}
                }
            })
        );
        for measurement in benchmark.measurements.values_mut() {
            measurement.label = "New display label".into();
        }
        let renamed = convert(&[benchmark]).unwrap();
        assert_eq!(export.metrics, renamed.metrics);
        assert_ne!(export.measures, renamed.measures);
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
        let export = convert(std::slice::from_ref(&benchmark)).unwrap();
        let gbps = export.metrics["gotatun-throughput"]["throughput-receiver"].value;
        assert!((gbps - 2.692_981_372_578_039).abs() < 1e-15);
        assert_eq!(benchmark.measurements["throughput.receiver"].value, bps);
        benchmark
            .measurements
            .get_mut("throughput.receiver")
            .unwrap()
            .value = 0.0;
        assert_eq!(
            convert(&[benchmark]).unwrap().metrics["gotatun-throughput"]["throughput-receiver"]
                .value,
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
            convert(&[benchmark.clone(), other.clone()])
                .unwrap()
                .metrics
                .len(),
            2
        );
        other.commit = "other-commit".into();
        assert!(convert(&[benchmark.clone(), other.clone()]).is_err());
        other.commit = benchmark.commit.clone();
        other
            .environment
            .insert("GITHUB_RUN_ATTEMPT".into(), "2".into());
        assert!(convert(&[benchmark, other]).is_err());
    }

    #[test]
    fn keeps_all_cpu_and_throughput_metrics_and_distinguishes_packet_loss() {
        let mut benchmark = fixture();
        for (id, label, unit, value) in [
            (
                "cpu.gotatun.down",
                "DOWN GotaTun CPU (% of one logical CPU)",
                Unit::Percent,
                371.5,
            ),
            (
                "cpu.iperf.up",
                "UP iperf CPU (% of one logical CPU)",
                Unit::Percent,
                30.0,
            ),
            (
                "cpu.iperf.down",
                "DOWN iperf CPU (% of one logical CPU)",
                Unit::Percent,
                40.0,
            ),
            (
                "throughput.sender",
                "Sender throughput",
                Unit::BitsPerSecond,
                2_800_000_000.0,
            ),
            ("udp.lost_percent", "UDP packet loss", Unit::Percent, 1.5),
        ] {
            benchmark.measurements.insert(
                benchy_lib::MetricId::new(id).unwrap(),
                Measurement {
                    label: label.into(),
                    unit,
                    value,
                },
            );
        }
        let export = convert(&[benchmark]).unwrap();
        assert_eq!(export.metrics.len(), 1);
        assert_eq!(export.metrics["gotatun-throughput"].len(), 7);
        assert_eq!(
            export.metrics["gotatun-throughput"]["cpu-gotatun-down"].value,
            371.5
        );
        assert_eq!(
            export.measures["cpu-gotatun-down"].name,
            "DOWN GotaTun CPU (logical CPU %)"
        );
        assert_eq!(
            export.measures["cpu-gotatun-down"].units,
            "% of one logical CPU"
        );
        assert_eq!(
            export.measures["udp-lost-percent"].name,
            "UDP packet loss (%)"
        );
        assert_eq!(export.measures["udp-lost-percent"].units, "%");
    }

    #[test]
    fn rejects_slug_collisions_and_conflicting_shared_measure_definitions() {
        let mut collision = fixture();
        collision.measurements.insert(
            benchy_lib::MetricId::new("cpu-gotatun-up").unwrap(),
            collision.measurements["cpu.gotatun.up"].clone(),
        );
        assert!(
            convert(&[collision])
                .unwrap_err()
                .to_string()
                .contains("collide")
        );

        let benchmark = fixture();
        let mut other = benchmark.clone();
        other.group.name = "another-benchmark".into();
        other.measurements.get_mut("cpu.gotatun.up").unwrap().unit = Unit::Count;
        assert!(
            convert(&[benchmark, other])
                .unwrap_err()
                .to_string()
                .contains("conflicting")
        );
    }

    #[test]
    fn rejects_empty_labels_and_overlong_bencher_identities() {
        for (name, id, label) in [
            ("x".repeat(1025), "test.value".into(), "Value".into()),
            ("test".into(), "x".repeat(65), "Value".into()),
            ("test".into(), "test.value".into(), "x".repeat(65)),
            ("test".into(), "test.value".into(), " ".into()),
        ] {
            let mut benchmark = fixture();
            benchmark.group.name = name;
            benchmark.measurements = BTreeMap::from([(
                benchy_lib::MetricId::new(id).unwrap(),
                Measurement {
                    label,
                    unit: Unit::Count,
                    value: 1.0,
                },
            )]);
            assert!(convert(&[benchmark]).is_err());
        }
    }
}
