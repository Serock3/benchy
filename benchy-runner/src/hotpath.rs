//! Converts hotpath-rs profiling reports into benchmark measurements.
//!
//! A benchmark host that runs hotpath-instrumented code writes a JSON report at the end of its
//! process. [`record_hotpath_report`] turns the per-function timings and lock statistics of that
//! report into measurements so data-path costs are tracked and plotted alongside the benchmark's
//! own metrics.
//!
//! Recorded metrics, all with an optional host segment prepended (`hotpath.<host>....`):
//!
//! - `hotpath.<path>.time` - a measured function's average time per call, in nanoseconds.
//!   Nanoseconds keep per-call values readable in Bencher instead of rounding to a fraction of a
//!   microsecond shown as seconds.
//! - `hotpath.lock.<name>.wait` / `hotpath.lock.<name>.hold` - a lock's average wait time before
//!   acquisition and average hold time, in nanoseconds. hotpath reports one entry per lock
//!   *instance*; instances of one creation site are merged so the metric tracks the site.
//! - Criterion benches profiled through hotpath's criterion adapter contribute their routine
//!   timing under `hotpath.<criterion benchmark id>.time`, with the id taken from the report's
//!   `user_metadata` (e.g. `blake2s_hash/blake2s_crate/32`).
//!
//! Call counts and allocation bytes are deliberately not recorded: the call count is a constant
//! of the workload definition, and an allocation-free data path only yields constant zeros. Rust
//! paths keep their shape with `::` mapped to `.` - the schema-v1 metric ID grammar does not
//! allow colons. Metric IDs must also fit the 64-character Bencher measure limit that
//! `benchy-cli export-bencher` enforces, so an overlong or otherwise unrepresentable name fails
//! the benchmark instead of silently dropping the function's metrics.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use benchy_lib::{MetricId, Unit};
use hotpath::json::JsonReport;

use crate::Recorder;

/// Bencher limits measure slugs to 64 characters, and export-bencher derives the slug from the
/// metric ID one character for one, so the ID itself must fit.
const MAX_METRIC_ID_LEN: usize = 64;

const METRIC_PREFIX: &str = "hotpath";

/// The routine entry hotpath's criterion adapter reports for every routine execution.
const CRITERION_ROUTINE: &str = "hotpath::criterion::routine";

/// The `user_metadata` key carrying the profiled criterion benchmark's full id.
const CRITERION_BENCHMARK_KEY: &str = "criterion_benchmark";

/// Parses a hotpath-rs JSON report such as the one written at process exit.
pub fn parse_hotpath_report(bytes: &[u8]) -> Result<JsonReport> {
    serde_json::from_slice(bytes).context("failed to deserialize a hotpath JSON report")
}

/// Records the average time per call of every measured function in one host's hotpath-rs JSON
/// report. `host` optionally prefixes the metric IDs, e.g. to keep two benchmark machines'
/// reports apart; `None` records unprefixed metrics.
pub fn record_hotpath_report(
    recorder: &mut Recorder,
    host: Option<&str>,
    report_bytes: &[u8],
) -> Result<()> {
    let report = parse_hotpath_report(report_bytes)?;
    record_hotpath_measurements(recorder, host, &report)
}

fn record_hotpath_measurements(
    recorder: &mut Recorder,
    host: Option<&str>,
    report: &JsonReport,
) -> Result<()> {
    let criterion_benchmark = report
        .user_metadata
        .as_ref()
        .and_then(|metadata| metadata.get(CRITERION_BENCHMARK_KEY))
        .cloned();
    if let Some(timing) = &report.functions_timing {
        for entry in &timing.data {
            // The guard's wrapper (the caller that built the report) is not product code.
            if entry.calls == 0 || entry.name == timing.caller_name {
                continue;
            }
            let (path, label) = if entry.name == CRITERION_ROUTINE {
                match &criterion_benchmark {
                    Some(benchmark) => (
                        criterion_path(benchmark),
                        format!("{} time per call", benchmark.replace('/', " ")),
                    ),
                    None => (
                        entry.name.to_lowercase().replace("::", "."),
                        label(host, &entry.name, "time per call"),
                    ),
                }
            } else {
                (
                    entry.name.to_lowercase().replace("::", "."),
                    label(host, &entry.name, "time per call"),
                )
            };
            match hotpath::parse_duration(&entry.total) {
                Some(total_ns) => {
                    recorder.dynamic_measurement(
                        &metric_id(host, &path)?,
                        &label,
                        Unit::Nanoseconds,
                        total_ns as f64 / entry.calls as f64,
                    )?;
                }
                None => {
                    eprintln!(
                        "::warning::cannot parse total time {:?} of {}",
                        entry.total, entry.name
                    );
                }
            }
        }
    }
    record_locks(recorder, host, report)
}

/// Records lock wait and hold times. hotpath reports one entry per lock instance with suffixed
/// labels (`name`, `name-2`, ...); instances created at one call site are merged into a single
/// metric so the metric set stays stable across runs that create a different number of locks.
fn record_locks(recorder: &mut Recorder, host: Option<&str>, report: &JsonReport) -> Result<()> {
    let Some(mutexes) = &report.mutexes else {
        return Ok(());
    };
    struct Site {
        name: String,
        acquisitions: u64,
        wait_ns: u128,
        hold_ns: u128,
    }
    let mut sites: BTreeMap<&str, Site> = BTreeMap::new();
    for entry in &mutexes.data {
        if entry.count == 0 {
            continue;
        }
        let site = sites.entry(entry.source.as_str()).or_insert_with(|| Site {
            name: base_lock_name(&entry.label).to_owned(),
            acquisitions: 0,
            wait_ns: 0,
            hold_ns: 0,
        });
        site.acquisitions += entry.count;
        match hotpath::parse_duration(&entry.wait_avg) {
            Some(wait_ns) => site.wait_ns += wait_ns as u128 * entry.count as u128,
            None => eprintln!(
                "::warning::cannot parse average wait time {:?} of lock {}",
                entry.wait_avg, entry.label
            ),
        }
        match hotpath::parse_duration(&entry.acquire_avg) {
            Some(hold_ns) => site.hold_ns += hold_ns as u128 * entry.count as u128,
            None => eprintln!(
                "::warning::cannot parse average hold time {:?} of lock {}",
                entry.acquire_avg, entry.label
            ),
        }
    }
    for site in sites.into_values() {
        let acquisitions = site.acquisitions as f64;
        recorder.dynamic_measurement(
            &lock_metric_id(host, &site.name, "wait")?,
            &lock_label(host, &site.name, "lock wait"),
            Unit::Nanoseconds,
            site.wait_ns as f64 / acquisitions,
        )?;
        recorder.dynamic_measurement(
            &lock_metric_id(host, &site.name, "hold")?,
            &lock_label(host, &site.name, "lock hold"),
            Unit::Nanoseconds,
            site.hold_ns as f64 / acquisitions,
        )?;
    }
    Ok(())
}

/// Strips the instance suffix hotpath appends to per-instance labels: `counter-2` -> `counter`.
fn base_lock_name(label: &str) -> &str {
    match label.rsplit_once('-') {
        Some((base, digits))
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        _ => label,
    }
}

/// Maps a criterion benchmark id such as `blake2s_hash/blake2s_crate/32` onto metric ID
/// segments. The grammar requires every segment to start with a lowercase letter, so purely
/// numeric parameters gain an `n` prefix (`32` -> `n32`).
fn criterion_path(benchmark: &str) -> String {
    benchmark
        .to_lowercase()
        .replace('/', ".")
        .split('.')
        .map(|segment| {
            if segment.starts_with(|byte: char| byte.is_ascii_digit()) {
                format!("n{segment}")
            } else {
                segment.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Builds the metric ID for one function: `hotpath.<host>.<path>.time`, or `hotpath.<path>.time`
/// without a host.
fn metric_id(host: Option<&str>, path: &str) -> Result<String> {
    let id = match host {
        Some(host) => format!("{METRIC_PREFIX}.{host}.{path}.time"),
        None => format!("{METRIC_PREFIX}.{path}.time"),
    };
    ensure!(
        id.len() <= MAX_METRIC_ID_LEN,
        "metric ID {id} is {} characters long, over the {MAX_METRIC_ID_LEN}-character Bencher measure limit",
        id.len()
    );
    MetricId::new(&id)
        .with_context(|| format!("cannot represent hotpath function {path:?} as a metric ID"))
        .map(|_| id)
}

/// Builds the metric ID for one lock: `hotpath.<host>.lock.<name>.<kind>`, or
/// `hotpath.lock.<name>.<kind>` without a host.
fn lock_metric_id(host: Option<&str>, name: &str, kind: &str) -> Result<String> {
    let id = match host {
        Some(host) => format!("{METRIC_PREFIX}.{host}.lock.{name}.{kind}"),
        None => format!("{METRIC_PREFIX}.lock.{name}.{kind}"),
    };
    ensure!(
        id.len() <= MAX_METRIC_ID_LEN,
        "metric ID {id} is {} characters long, over the {MAX_METRIC_ID_LEN}-character Bencher measure limit",
        id.len()
    );
    MetricId::new(&id)
        .with_context(|| format!("cannot represent hotpath lock {name:?} as a metric ID"))
        .map(|_| id)
}

/// Builds a human-facing label from the host and the function's short name, e.g.
/// `alice format_packet_data time per call`. The export suffix (unit) and the 64-character
/// Bencher measure name limit leave room for the full short name.
fn label(host: Option<&str>, name: &str, what: &str) -> String {
    let short = name.rsplit("::").next().unwrap_or(name);
    match host {
        Some(host) => format!("{host} {short} {what}"),
        None => format!("{short} {what}"),
    }
}

/// Builds a human-facing label for a lock metric, e.g. `receiving-key-counter lock wait`.
fn lock_label(host: Option<&str>, name: &str, what: &str) -> String {
    match host {
        Some(host) => format!("{host} {name} {what}"),
        None => format!("{name} {what}"),
    }
}

#[cfg(test)]
mod tests {
    use benchy_lib::MetricId;

    use super::*;
    use crate::{BenchmarkDefinition, Recorder};

    const DEFINITION: BenchmarkDefinition = BenchmarkDefinition {
        repository: "gotatun",
        name: "gotatun-hotpath",
        description: "test",
    };

    fn recorder() -> Recorder {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(Recorder::new(DEFINITION, "/dev/null"))
    }

    // A trimmed copy of a real report produced by hotpath 0.30.1. The alloc section is
    // intentionally present: it must be ignored.
    const REPORT: &str = r#"{
        "type": "hotpath_report",
        "version": "0.30.1",
        "meta": {
            "rustc": "1.95.0",
            "os": "linux-x86_64",
            "created_at": "2026-10-29T17:13:25Z",
            "run": "native"
        },
        "functions_timing": {
            "profiling_mode": "timing",
            "time_elapsed": "10.00 ms",
            "total_elapsed_ns": 10000000,
            "description": "Function execution time metrics.",
            "caller_name": "endpoint",
            "percentiles": [95.0],
            "data": [
                {
                    "id": 0,
                    "name": "endpoint",
                    "calls": 1,
                    "sampled_calls": 1,
                    "avg": "45.00 s",
                    "p95": "45.00 s",
                    "total": "45.00 s",
                    "percent_total": "100.00%",
                    "location": { "file": "src/main.rs", "line": 1, "column": 1 }
                },
                {
                    "id": 1,
                    "name": "gotatun::noise::session::format_packet_data",
                    "calls": 10,
                    "sampled_calls": 10,
                    "avg": "1.00 ms",
                    "p95": "1.10 ms",
                    "total": "10.00 ms",
                    "percent_total": "100.00%",
                    "location": { "file": "src/noise/session.rs", "line": 229, "column": 5 }
                }
            ],
            "total_count": 1,
            "included_count": 1
        },
        "functions_alloc": {
            "profiling_mode": "alloc-bytes",
            "time_elapsed": "10.00 ms",
            "total_elapsed_ns": 10000000,
            "total_allocated": "1000 B",
            "description": "Exclusive allocation bytes by each function (excluding nested calls).",
            "caller_name": "endpoint",
            "percentiles": [95.0],
            "data": [
                {
                    "id": 1,
                    "name": "gotatun::noise::session::format_packet_data",
                    "calls": 10,
                    "sampled_calls": 10,
                    "avg": "100 B",
                    "p95": "100 B",
                    "total": "1000 B",
                    "percent_total": "100.00%",
                    "location": { "file": "src/noise/session.rs", "line": 229, "column": 5 }
                }
            ],
            "total_count": 1,
            "included_count": 1
        },
        "mutexes": {
            "current_elapsed_ns": 10000000,
            "percentiles": [95.0],
            "data": [
                {
                    "id": 1,
                    "source": "src/noise/session.rs:202",
                    "label": "receiving-key-counter",
                    "has_custom_label": true,
                    "type_name": "gotatun::noise::session::ReceivingKeyCounterValidator",
                    "count": 10,
                    "sampled_count": 10,
                    "wait_avg": "2 ns",
                    "acquire_avg": "8 ns",
                    "wait_percentiles": { "p95": "3 ns" },
                    "acquire_percentiles": { "p95": "9 ns" },
                    "location": { "file": "src/noise/session.rs", "line": 202, "column": 36 },
                    "iter": 0
                },
                {
                    "id": 2,
                    "source": "src/noise/session.rs:202",
                    "label": "receiving-key-counter-2",
                    "has_custom_label": true,
                    "type_name": "gotatun::noise::session::ReceivingKeyCounterValidator",
                    "count": 30,
                    "sampled_count": 30,
                    "wait_avg": "4 ns",
                    "acquire_avg": "10 ns",
                    "wait_percentiles": { "p95": "5 ns" },
                    "acquire_percentiles": { "p95": "11 ns" },
                    "location": { "file": "src/noise/session.rs", "line": 202, "column": 36 },
                    "iter": 1
                }
            ],
            "total_count": 2,
            "included_count": 2
        }
    }"#;

    #[test]
    fn records_average_time_per_call_in_nanoseconds() {
        let mut recorder = recorder();
        record_hotpath_report(&mut recorder, Some("alice"), REPORT.as_bytes()).unwrap();

        let measurements = &recorder.benchmark.measurements;
        assert_eq!(measurements.len(), 3);
        let id =
            MetricId::new("hotpath.alice.gotatun.noise.session.format_packet_data.time").unwrap();
        let time = &measurements[&id];
        assert_eq!(time.value, 1_000_000.0);
        assert_eq!(time.unit, Unit::Nanoseconds);
        assert_eq!(time.label, "alice format_packet_data time per call");
    }

    #[test]
    fn merges_lock_instances_by_creation_site() {
        let mut recorder = recorder();
        record_hotpath_report(&mut recorder, None, REPORT.as_bytes()).unwrap();

        let measurements = &recorder.benchmark.measurements;
        // Two lock instances of one site merge into one wait and one hold metric.
        let wait =
            &measurements[&MetricId::new("hotpath.lock.receiving-key-counter.wait").unwrap()];
        assert_eq!(wait.value, 3.5);
        assert_eq!(wait.unit, Unit::Nanoseconds);
        assert_eq!(wait.label, "receiving-key-counter lock wait");
        let hold =
            &measurements[&MetricId::new("hotpath.lock.receiving-key-counter.hold").unwrap()];
        assert_eq!(hold.value, 9.5);
        assert_eq!(hold.label, "receiving-key-counter lock hold");
        assert!(
            measurements
                .get(&MetricId::new("hotpath.lock.receiving-key-counter-2.wait").unwrap())
                .is_none()
        );
    }

    #[test]
    fn host_prefix_is_optional() {
        let mut recorder = recorder();
        record_hotpath_report(&mut recorder, None, REPORT.as_bytes()).unwrap();

        let measurements = &recorder.benchmark.measurements;
        let id = MetricId::new("hotpath.gotatun.noise.session.format_packet_data.time").unwrap();
        assert!(measurements.contains_key(&id));
        assert_eq!(measurements[&id].label, "format_packet_data time per call");
    }

    // The shape hotpath's criterion adapter writes: one routine entry, the benchmark id in the
    // report's user metadata, and the guard's wrapper as the section's caller name.
    const CRITERION_REPORT: &str = r#"{
        "type": "hotpath_report",
        "version": "0.30.1",
        "meta": {
            "rustc": "1.95.0",
            "os": "linux-x86_64",
            "created_at": "2026-10-29T18:40:00Z",
            "run": "native"
        },
        "user_metadata": {
            "criterion_benchmark": "blake2s_hash/blake2s_crate/32",
            "criterion_iterations": "1000"
        },
        "functions_timing": {
            "profiling_mode": "timing",
            "time_elapsed": "10.00 ms",
            "total_elapsed_ns": 10000000,
            "description": "Function execution time metrics.",
            "caller_name": "hotpath::criterion::bench",
            "percentiles": [95.0],
            "data": [
                {
                    "id": 0,
                    "name": "hotpath::criterion::bench",
                    "calls": 1,
                    "sampled_calls": 1,
                    "avg": "10.00 ms",
                    "p95": "10.00 ms",
                    "total": "10.00 ms",
                    "percent_total": "100.00%",
                    "location": { "file": "src/main.rs", "line": 1, "column": 1 }
                },
                {
                    "id": 1,
                    "name": "hotpath::criterion::routine",
                    "calls": 1000,
                    "sampled_calls": 1000,
                    "avg": "10.00 µs",
                    "p95": "10.00 µs",
                    "total": "10.00 ms",
                    "percent_total": "100.00%",
                    "location": { "file": "src/main.rs", "line": 8, "column": 4 }
                }
            ],
            "total_count": 2,
            "included_count": 2
        }
    }"#;

    #[test]
    fn records_criterion_routine_timings() {
        let mut recorder = recorder();
        record_hotpath_report(&mut recorder, None, CRITERION_REPORT.as_bytes()).unwrap();

        let measurements = &recorder.benchmark.measurements;
        assert_eq!(measurements.len(), 1);
        let routine =
            &measurements[&MetricId::new("hotpath.blake2s_hash.blake2s_crate.n32.time").unwrap()];
        assert_eq!(routine.value, 10_000.0);
        assert_eq!(routine.unit, Unit::Nanoseconds);
        assert_eq!(routine.label, "blake2s_hash blake2s_crate 32 time per call");
    }

    #[test]
    fn criterion_ids_map_onto_valid_metric_segments() {
        assert_eq!(
            criterion_path("blake2s_hash/blake2s_crate/32"),
            "blake2s_hash.blake2s_crate.n32"
        );
        assert_eq!(
            criterion_path("assemble_ipv4_fragment_interleaved/16x4000"),
            "assemble_ipv4_fragment_interleaved.n16x4000"
        );
        // Not numeric: the segment keeps its shape.
        assert_eq!(
            criterion_path("chacha20poly1305/ring/1400"),
            "chacha20poly1305.ring.n1400"
        );
    }

    #[test]
    fn rejects_overlong_function_paths() {
        // 51 characters plus prefix, host, and kind: over 64 after mapping.
        let long = "gotatun.a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p.q.r.s.verylongfunctionname";
        let error = metric_id(Some("alice"), long).unwrap_err().to_string();
        assert!(error.contains("Bencher measure limit"), "{error}");
    }

    #[test]
    fn rejects_unrepresentable_function_names() {
        let error = metric_id(Some("alice"), "gotatun.generic.<u8>.work")
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot represent"), "{error}");
    }

    #[test]
    fn parses_a_real_report_shape() {
        let report = parse_hotpath_report(REPORT.as_bytes()).unwrap();
        let timing = report.functions_timing.unwrap();
        assert_eq!(timing.data.len(), 2);
        assert_eq!(
            timing.data[1].name,
            "gotatun::noise::session::format_packet_data"
        );
    }

    #[test]
    fn labels_use_the_short_function_name() {
        assert_eq!(
            label(
                Some("bob"),
                "gotatun::noise::handle_incoming_packet",
                "time per call"
            ),
            "bob handle_incoming_packet time per call"
        );
        assert_eq!(
            label(
                None,
                "gotatun::noise::handle_incoming_packet",
                "time per call"
            ),
            "handle_incoming_packet time per call"
        );
    }

    #[test]
    fn lock_names_strip_hotpath_instance_suffixes() {
        assert_eq!(
            base_lock_name("receiving-key-counter"),
            "receiving-key-counter"
        );
        assert_eq!(
            base_lock_name("receiving-key-counter-2"),
            "receiving-key-counter"
        );
        // Only an all-digit suffix is an instance marker.
        assert_eq!(base_lock_name("channel-v6"), "channel-v6");
    }
}
