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
    #[serde(skip_serializing_if = "Option::is_none")]
    lower_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upper_value: Option<f64>,
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
            !parent.starts_with(&result_dir),
            "--output must be outside the result directory"
        );
        let destination = parent.join(output.file_name().context("output must name a file")?);
        ensure!(
            !destinations.contains(&destination),
            "metrics and measure definitions must use different output files"
        );
        destinations.push(destination);
    }

    let samples = read_samples(&result_dir)?;
    let benchmarks: Vec<_> = samples.iter().flatten().cloned().collect();
    let first = benchmarks
        .first()
        .context("no benchmark results provided")?;
    ensure!(
        benchmarks
            .iter()
            .all(|benchmark| same_run(first, benchmark)),
        "result files must belong to the same repository, commit, branch, and workflow attempt"
    );
    if let Some(path) = publication_context {
        let context: crate::github::Publication = serde_json::from_slice(&fs::read(path)?)
            .context("invalid trusted publication context")?;
        context.verify(&benchmarks)?;
    }
    let export = aggregate_samples(&samples)?;
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

fn json_paths(directory: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut paths = fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.retain(|path| {
        path.extension().is_some_and(|ext| ext == "json")
            && path
                .file_name()
                .is_some_and(|name| name != crate::SAMPLE_MANIFEST)
    });
    paths.sort();
    Ok(paths)
}

fn read_samples(directory: &Path) -> Result<Vec<Vec<Benchmark>>> {
    let manifest_path = directory.join(crate::SAMPLE_MANIFEST);
    let manifest = if manifest_path.exists() {
        let manifest: crate::SampleManifest =
            serde_json::from_slice(&fs::read(manifest_path)?).context("invalid sample manifest")?;
        manifest.validate()?;
        Some(manifest)
    } else {
        None
    };
    let iterations = manifest.as_ref().map_or(1, |manifest| manifest.iterations);
    let expected_directories: std::collections::BTreeSet<_> = if iterations > 1 {
        (1..=iterations)
            .map(|index| format!("sample-{index:03}"))
            .collect()
    } else {
        Default::default()
    };
    let actual_directories = fs::read_dir(directory)?
        .map(|entry| {
            let entry = entry?;
            Ok((
                entry.file_name().to_string_lossy().into_owned(),
                entry.file_type()?.is_dir(),
            ))
        })
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(name, is_dir)| *is_dir && name.starts_with("sample-"))
        .map(|(name, _)| name)
        .collect();
    ensure!(
        expected_directories == actual_directories,
        "sample directories do not match the declared iteration count"
    );
    if iterations > 1 {
        ensure!(
            json_paths(directory)?.is_empty(),
            "cannot mix flat results and repeated samples"
        );
    }
    let mut samples = Vec::new();
    for index in 1..=iterations {
        let path = manifest.as_ref().map_or_else(
            || directory.to_owned(),
            |manifest| manifest.directory(directory, index),
        );
        let paths = json_paths(&path)?;
        ensure!(
            !paths.is_empty(),
            "no JSON result files found in {}",
            path.display()
        );
        let benchmarks = paths
            .iter()
            .map(|path| {
                let bytes = fs::read(path)?;
                serde_json::from_slice::<Benchmark>(&bytes)
                    .with_context(|| format!("invalid benchmark result {}", path.display()))
            })
            .collect::<Result<Vec<_>>>()?;
        if let Some(manifest) = &manifest {
            let names = benchmarks
                .iter()
                .map(|benchmark| benchmark.group.name.clone())
                .collect();
            ensure!(
                manifest.benchmarks == names,
                "sample {index} is missing or has unexpected benchmarks"
            );
            for benchmark in &benchmarks {
                ensure!(
                    benchmark.environment.get("BENCHY_SAMPLE_INDEX") == Some(&index.to_string())
                        && benchmark.environment.get("BENCHY_SAMPLE_COUNT")
                            == Some(&iterations.to_string()),
                    "sample index/count does not match its manifest"
                );
            }
        }
        samples.push(benchmarks);
    }
    Ok(samples)
}

fn same_run(first: &Benchmark, benchmark: &Benchmark) -> bool {
    benchmark.group.repository == first.group.repository
        && benchmark.commit == first.commit
        && benchmark.branch == first.branch
        && benchmark.run_id == first.run_id
        && benchmark.environment.get("GITHUB_RUN_ATTEMPT")
            == first.environment.get("GITHUB_RUN_ATTEMPT")
}

fn aggregate_samples(samples: &[Vec<Benchmark>]) -> Result<Export> {
    let mut exports = samples
        .iter()
        .map(|sample| convert(sample))
        .collect::<Result<Vec<_>>>()?;
    ensure!(!exports.is_empty(), "no benchmark samples provided");
    let mut export = exports.remove(0);
    for (sample, converted) in samples.iter().skip(1).zip(&exports) {
        ensure!(
            converted.measures == export.measures
                && converted.metrics.keys().eq(export.metrics.keys()),
            "samples have different benchmarks or measure definitions"
        );
        for benchmark in sample {
            let first = samples[0]
                .iter()
                .find(|first| first.group == benchmark.group)
                .context("samples have different benchmarks")?;
            ensure!(
                same_run(first, benchmark) && first.parameters == benchmark.parameters,
                "samples have different source identities or benchmark parameters"
            );
        }
    }
    for converted in &exports {
        for (name, values) in &export.metrics {
            ensure!(
                values.keys().eq(converted.metrics[name].keys()),
                "samples have different measurement sets"
            );
        }
    }
    if samples.len() > 1 {
        for (name, measures) in &mut export.metrics {
            for (slug, metric) in measures {
                let values = std::iter::once(&*metric)
                    .chain(exports.iter().map(|sample| &sample.metrics[name][slug]));
                *metric = summarize(values)?;
            }
        }
    }
    Ok(export)
}

fn summarize<'a>(values: impl Iterator<Item = &'a Metric>) -> Result<Metric> {
    let (mut count, mut mean, mut error_norm) = (0_u32, 0.0, Some(0.0_f64));
    for metric in values {
        count += 1;
        let delta = metric.value - mean;
        mean += delta / f64::from(count);
        error_norm = error_norm
            .zip(metric.upper_value)
            .map(|(norm, upper)| norm.hypot(upper - metric.value));
    }
    ensure!(count >= 2, "aggregation requires at least two samples");
    // Independent estimates of equal-weight run means: SE(mean) = sqrt(sum(SE_i^2))/N.
    // Missing uncertainty in any run leaves the aggregate without bounds, including CPU.
    let error = error_norm.map(|norm| norm / f64::from(count));
    let lower = error.map(|error| mean - error);
    let upper = error.map(|error| mean + error);
    ensure!(
        mean.is_finite() && lower.is_none_or(f64::is_finite) && upper.is_none_or(f64::is_finite),
        "non-finite sample statistics"
    );
    Ok(Metric {
        value: mean,
        lower_value: lower,
        upper_value: upper,
    })
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
            same_run(first, benchmark),
            "result files must belong to the same repository, commit, branch, and workflow attempt"
        );
        ensure!(
            !benchmark.measurements.is_empty() || !benchmark.cases.is_empty(),
            "benchmark {}/{} has no measurements",
            group.repository,
            group.name
        );
        ensure!(
            benchmark.cases.keys().all(|case| !case.trim().is_empty()),
            "benchmark case must have a name"
        );
        let groups = std::iter::once((group.name.clone(), &benchmark.measurements))
            .filter(|(_, measurements)| !measurements.is_empty())
            .chain(
                benchmark
                    .cases
                    .iter()
                    .map(|(case, measurements)| (format!("{}/{case}", group.name), measurements)),
            );
        for (name, measurements) in groups {
            ensure!(
                !measurements.is_empty(),
                "benchmark case {name} has no measurements"
            );
            ensure!(!name.ends_with('/'), "benchmark case must have a name");
            ensure!(
                name.chars().count() <= 1024,
                "Bencher benchmark name exceeds 1024 characters: {name}"
            );
            let mut values = BTreeMap::new();
            for (id, measurement) in measurements {
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
                let error = measurement
                    .uncertainty
                    .as_ref()
                    .map(|uncertainty| {
                        ensure!(
                            uncertainty.is_valid(),
                            "measurement {name}/{id} has invalid uncertainty"
                        );
                        let divisor = if measurement.unit == Unit::BitsPerSecond {
                            1_000_000_000.0
                        } else {
                            1.0
                        };
                        Ok(uncertainty.standard_error / divisor)
                    })
                    .transpose()?;
                let lower_value = error.map(|error| value - error);
                let upper_value = error.map(|error| value + error);
                ensure!(
                    lower_value.is_none_or(f64::is_finite)
                        && upper_value.is_none_or(f64::is_finite),
                    "measurement {name}/{id} has non-finite bounds"
                );
                if let Some(previous) = measures.insert(slug.clone(), definition) {
                    ensure!(
                        previous == measures[&slug],
                        "conflicting names or units for Bencher measure {slug}"
                    );
                }
                values.insert(
                    slug,
                    Metric {
                        value,
                        lower_value,
                        upper_value,
                    },
                );
            }
            ensure!(
                metrics.insert(name.clone(), values).is_none(),
                "duplicate benchmark: {name}"
            );
        }
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
        Unit::Nanoseconds => ("nanoseconds", "ns", measurement.value),
        Unit::Count => ("count", "count", measurement.value),
        Unit::Bytes => ("bytes", "B", measurement.value),
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
    fn repeated_runs_propagate_interval_errors_and_leave_cpu_without_bounds() {
        let samples: Vec<_> = [1.0, 2.0, 3.0]
            .into_iter()
            .map(|value| {
                let mut benchmark = fixture();
                let throughput = benchmark
                    .measurements
                    .get_mut("throughput.receiver")
                    .unwrap();
                throughput.value = value * 1_000_000_000.0;
                throughput.uncertainty = Some(benchy_lib::Uncertainty {
                    method: benchy_lib::UncertaintyMethod::IndependentIntervals,
                    sample_count: 30,
                    standard_error: 100_000_000.0,
                });
                benchmark
                    .measurements
                    .get_mut("cpu.gotatun.up")
                    .unwrap()
                    .value = value * 100.0;
                vec![benchmark]
            })
            .collect();
        let export = aggregate_samples(&samples).unwrap();
        let throughput = &export.metrics["gotatun-throughput"]["throughput-receiver"];
        assert_eq!(throughput.value, 2.0);
        let expected_error = 0.1 / 3.0_f64.sqrt();
        assert!((throughput.upper_value.unwrap() - (2.0 + expected_error)).abs() < 1e-12);
        assert!((throughput.lower_value.unwrap() - (2.0 - expected_error)).abs() < 1e-12);
        assert_eq!(
            serde_json::to_value(&export.metrics["gotatun-throughput"]["cpu-gotatun-up"]).unwrap(),
            json!({"value": 200.0})
        );
        let single = aggregate_samples(&samples[..1]).unwrap();
        assert_eq!(single, convert(&samples[0]).unwrap());
        let mut missing = samples.clone();
        missing[1][0]
            .measurements
            .get_mut("throughput.receiver")
            .unwrap()
            .uncertainty = None;
        assert!(aggregate_samples(&missing).unwrap().metrics["gotatun-throughput"]["throughput-receiver"].lower_value.is_none());
    }

    #[test]
    fn cases_share_measures_and_validate_repeated_case_sets() {
        let mut first = fixture();
        first.measurements.clear();
        let measurement = Measurement {
            label: "Time per call".into(),
            unit: Unit::Nanoseconds,
            value: 100.0,
            uncertainty: None,
        };
        for case in ["encrypt", "decrypt"] {
            first.cases.insert(
                case.into(),
                BTreeMap::from([(
                    benchy_lib::MetricId::new("time").unwrap(),
                    measurement.clone(),
                )]),
            );
        }
        let export = convert(&[first.clone()]).unwrap();
        assert_eq!(export.measures.len(), 1);
        assert_eq!(
            export.metrics["gotatun-throughput/encrypt"]["time"].value,
            100.0
        );
        let mut second = first.clone();
        second
            .cases
            .get_mut("encrypt")
            .unwrap()
            .get_mut("time")
            .unwrap()
            .value = 200.0;
        let aggregate = aggregate_samples(&[vec![first.clone()], vec![second.clone()]]).unwrap();
        assert_eq!(
            aggregate.metrics["gotatun-throughput/encrypt"]["time"].value,
            150.0
        );
        second.cases.remove("decrypt");
        assert!(aggregate_samples(&[vec![first.clone()], vec![second]]).is_err());
        first.cases.insert(" ".into(), BTreeMap::new());
        assert!(convert(&[first]).is_err());
    }

    #[test]
    fn repeated_runs_reject_incomparable_or_failed_samples() {
        let first = fixture();
        let mut changed = first.clone();
        changed
            .parameters
            .insert("duration_seconds".into(), "60".into());
        assert!(aggregate_samples(&[vec![first.clone()], vec![changed]]).is_err());
        let mut changed = first.clone();
        changed.measurements.remove("cpu.gotatun.up");
        assert!(aggregate_samples(&[vec![first.clone()], vec![changed]]).is_err());
        let mut changed = first.clone();
        changed.commit = "different".into();
        assert!(aggregate_samples(&[vec![first.clone()], vec![changed]]).is_err());
        let mut changed = first.clone();
        changed.status = BenchmarkStatus::Failed;
        assert!(aggregate_samples(&[vec![first], vec![changed]]).is_err());
        let huge = [
            Metric {
                value: f64::MAX,
                lower_value: None,
                upper_value: None,
            },
            Metric {
                value: -f64::MAX,
                lower_value: None,
                upper_value: None,
            },
        ];
        assert!(summarize(huge.iter()).is_err());
    }

    #[test]
    fn single_run_exports_estimated_standard_error_and_rejects_invalid_uncertainty() {
        let mut benchmark = fixture();
        benchmark
            .measurements
            .get_mut("throughput.receiver")
            .unwrap()
            .uncertainty = Some(benchy_lib::Uncertainty {
            method: benchy_lib::UncertaintyMethod::IndependentIntervals,
            sample_count: 30,
            standard_error: 50_000_000.0,
        });
        let export = convert(&[benchmark.clone()]).unwrap();
        assert_eq!(
            serde_json::to_value(export.metrics).unwrap(),
            json!({"gotatun-throughput": {
                "throughput-receiver": {"value": 2.73, "lower_value": 2.68, "upper_value": 2.78},
                "cpu-gotatun-up": {"value": 282.1}
            }})
        );
        for (count, error) in [(1, 1.0), (30, -1.0), (30, f64::NAN), (30, f64::INFINITY)] {
            let mut invalid = benchmark.clone();
            let uncertainty = invalid
                .measurements
                .get_mut("throughput.receiver")
                .unwrap()
                .uncertainty
                .as_mut()
                .unwrap();
            uncertainty.sample_count = count;
            uncertainty.standard_error = error;
            assert!(convert(&[invalid]).is_err());
        }
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
                uncertainty: None,
            },
        );
        benchmark.measurements.insert(
            benchy_lib::MetricId::new("packets.received").unwrap(),
            Measurement {
                label: "Received packets".into(),
                unit: Unit::Count,
                value: 42.0,
                uncertainty: None,
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
            (
                "hotpath.receive_packet_data",
                "receive_packet_data time per call",
                Unit::Nanoseconds,
                753.7,
            ),
        ] {
            benchmark.measurements.insert(
                benchy_lib::MetricId::new(id).unwrap(),
                Measurement {
                    label: label.into(),
                    unit,
                    value,
                    uncertainty: None,
                },
            );
        }
        let export = convert(&[benchmark]).unwrap();
        assert_eq!(export.metrics.len(), 1);
        assert_eq!(export.metrics["gotatun-throughput"].len(), 8);
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
        assert_eq!(
            export.measures["hotpath-receive-packet-data"].name,
            "receive_packet_data time per call (ns)"
        );
        assert_eq!(
            export.measures["hotpath-receive-packet-data"].units,
            "nanoseconds"
        );
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
                    uncertainty: None,
                },
            )]);
            assert!(convert(&[benchmark]).is_err());
        }
    }
}
