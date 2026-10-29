//! Converts hotpath-rs profiling reports into benchmark measurements.
//!
//! A benchmark host that runs hotpath-instrumented code writes a JSON report at the end of its
//! process. [`record_hotpath_report`] turns the per-function timings of that report into
//! measurements so data-path costs are tracked and plotted alongside the benchmark's own
//! metrics.
//!
//! Every measured function contributes one metric, `hotpath.<path>.time` with an optional host
//! segment prepended (`hotpath.<host>.<path>.time`): the average time per call in nanoseconds.
//! Nanoseconds keep per-call values readable in Bencher instead of rounding to a fraction of a
//! microsecond shown as seconds.
//!
//! Call counts and allocation bytes are deliberately not recorded: the call count is a constant
//! of the workload definition, and an allocation-free data path only yields constant zeros. Rust
//! paths keep their shape with `::` mapped to `.` - the schema-v1 metric ID grammar does not
//! allow colons. Metric IDs must also fit the 64-character Bencher measure limit that
//! `benchy-cli export-bencher` enforces, so an overlong or otherwise unrepresentable name
//! fails the benchmark instead of silently dropping the function's metrics.

use anyhow::{Context, Result, ensure};
use benchy_lib::{MetricId, Unit};
use hotpath::json::JsonReport;

use crate::Recorder;

/// Bencher limits measure slugs to 64 characters, and export-bencher derives the slug from the
/// metric ID one character for one, so the ID itself must fit.
const MAX_METRIC_ID_LEN: usize = 64;

const METRIC_PREFIX: &str = "hotpath";

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
    if let Some(timing) = &report.functions_timing {
        for entry in &timing.data {
            // The guard's wrapper (the caller that built the report) is not product code.
            if entry.calls == 0 || entry.name == timing.caller_name {
                continue;
            }
            match hotpath::parse_duration(&entry.total) {
                Some(total_ns) => {
                    recorder.dynamic_measurement(
                        &metric_id(host, &entry.name)?,
                        &label(host, &entry.name, "time per call"),
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
    Ok(())
}

/// Builds the metric ID for one function: `hotpath.<host>.<path>.time`, or `hotpath.<path>.time`
/// without a host.
fn metric_id(host: Option<&str>, name: &str) -> Result<String> {
    let path = name.to_lowercase().replace("::", ".");
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
        .with_context(|| format!("cannot represent hotpath function {name:?} as a metric ID"))
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
        }
    }"#;

    #[test]
    fn records_average_time_per_call_in_nanoseconds() {
        let mut recorder = recorder();
        record_hotpath_report(&mut recorder, Some("alice"), REPORT.as_bytes()).unwrap();

        let measurements = &recorder.benchmark.measurements;
        assert_eq!(measurements.len(), 1);
        let id =
            MetricId::new("hotpath.alice.gotatun.noise.session.format_packet_data.time").unwrap();
        let time = &measurements[&id];
        assert_eq!(time.value, 1_000_000.0);
        assert_eq!(time.unit, Unit::Nanoseconds);
        assert_eq!(time.label, "alice format_packet_data time per call");
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

    #[test]
    fn rejects_overlong_function_paths() {
        // 51 characters plus prefix, host, and kind: over 64 after mapping.
        let long = "gotatun::a::b::c::d::e::f::g::h::i::j::k::l::m::n::o::p::q::r::s::verylongname";
        let error = metric_id(Some("alice"), long).unwrap_err().to_string();
        assert!(error.contains("Bencher measure limit"), "{error}");
    }

    #[test]
    fn rejects_unrepresentable_function_names() {
        let error = metric_id(Some("alice"), "gotatun::generic::<u8>::work")
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
}
