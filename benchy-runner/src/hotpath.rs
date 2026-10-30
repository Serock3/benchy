//! Convert hotpath reports into named function/lock cases with shared measures.
//! Function timings are inclusive; allocation bytes are exclusive. Serial lock waits
//! measure uncontended acquisition. Full names, counts and settings stay in the raw profile.
use crate::{MeasurementDefinition, Recorder};
use anyhow::{Context, Result, ensure};
use benchy_lib::Unit;
use hotpath::json::JsonReport;
use std::collections::BTreeMap;

pub fn parse_hotpath_report(bytes: &[u8]) -> Result<JsonReport> {
    serde_json::from_slice(bytes).context("failed to deserialize a hotpath JSON report")
}

pub fn record_hotpath_report(
    recorder: &mut Recorder,
    host: Option<&str>,
    bytes: &[u8],
) -> Result<()> {
    let report = parse_hotpath_report(bytes)?;
    let timing = report
        .functions_timing
        .as_ref()
        .context("hotpath report has no function timings")?;
    let mut measured = 0;
    for entry in &timing.data {
        if entry.calls == 0 || entry.name == timing.caller_name {
            continue;
        }
        let total =
            hotpath::parse_duration(&entry.total).context("invalid hotpath timing total")?;
        recorder.case_measurement(
            &function_case(host, &entry.name),
            MeasurementDefinition {
                id: "time",
                label: "Time per call",
                unit: Unit::Nanoseconds,
            },
            total as f64 / entry.calls as f64,
        )?;
        measured += 1;
    }
    ensure!(
        measured > 0,
        "hotpath report contains no measured function calls"
    );
    if let Some(allocations) = &report.functions_alloc {
        for entry in &allocations.data {
            if entry.calls == 0 || entry.name == allocations.caller_name {
                continue;
            }
            let total =
                hotpath::parse_bytes(&entry.total).context("invalid hotpath allocation total")?;
            recorder.case_measurement(
                &function_case(host, &entry.name),
                MeasurementDefinition {
                    id: "allocated",
                    label: "Allocated per call",
                    unit: Unit::Bytes,
                },
                total as f64 / entry.calls as f64,
            )?;
        }
    }
    let Some(mutexes) = report.mutexes else {
        return Ok(());
    };
    // Merge instances by creation site, weighting by acquisition count.
    let mut sites = BTreeMap::new();
    for entry in &mutexes.data {
        if entry.count == 0 {
            continue;
        }
        let site = sites.entry(&entry.source).or_insert((
            base_lock_name(&entry.label),
            0_u64,
            0_u128,
            0_u128,
        ));
        site.1 += entry.count;
        site.2 += hotpath::parse_duration(&entry.wait_avg).context("invalid lock wait")? as u128
            * entry.count as u128;
        site.3 += hotpath::parse_duration(&entry.acquire_avg).context("invalid lock hold")? as u128
            * entry.count as u128;
    }
    for (name, count, wait, hold) in sites.into_values() {
        let case = case_name(host, &format!("lock/{name}"));
        recorder.case_measurement(
            &case,
            MeasurementDefinition {
                id: "lock.wait",
                label: "Lock wait",
                unit: Unit::Nanoseconds,
            },
            wait as f64 / count as f64,
        )?;
        recorder.case_measurement(
            &case,
            MeasurementDefinition {
                id: "lock.hold",
                label: "Lock hold",
                unit: Unit::Nanoseconds,
            },
            hold as f64 / count as f64,
        )?;
    }
    Ok(())
}

fn function_case(host: Option<&str>, name: &str) -> String {
    case_name(host, name.rsplit("::").next().unwrap_or(name))
}
fn case_name(host: Option<&str>, name: &str) -> String {
    host.map_or_else(|| name.to_owned(), |host| format!("{host}/{name}"))
}
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BenchmarkDefinition;
    fn recorder() -> Recorder {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(Recorder::new(
                BenchmarkDefinition {
                    repository: "gotatun",
                    name: "gotatun-hotpath",
                    description: "test",
                },
                "/dev/null",
            ))
    }
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
    fn functions_share_time_and_allocation_measures() {
        let mut recorder = recorder();
        record_hotpath_report(&mut recorder, None, REPORT.as_bytes()).unwrap();
        assert!(recorder.benchmark.measurements.is_empty());
        let values = &recorder.benchmark.cases["format_packet_data"];
        assert_eq!(values["time"].value, 1_000_000.0);
        assert_eq!(values["time"].unit, Unit::Nanoseconds);
        assert_eq!(values["allocated"].value, 100.0);
        assert_eq!(values["allocated"].unit, Unit::Bytes);
    }
    #[test]
    fn locks_are_weighted_across_instances() {
        let mut recorder = recorder();
        record_hotpath_report(&mut recorder, Some("alice"), REPORT.as_bytes()).unwrap();
        let values = &recorder.benchmark.cases["alice/lock/receiving-key-counter"];
        assert_eq!(values["lock.wait"].value, 3.5);
        assert_eq!(values["lock.hold"].value, 9.5);
    }
    #[test]
    fn rejects_empty_and_invalid_reports() {
        let mut recorder = recorder();
        assert!(record_hotpath_report(&mut recorder, None, b"{}").is_err());
        assert!(
            record_hotpath_report(
                &mut recorder,
                None,
                REPORT.replace("10.00 ms", "invalid").as_bytes()
            )
            .is_err()
        );
    }
    #[test]
    fn duplicate_short_function_names_fail_instead_of_merging() {
        let mut recorder = recorder();
        record_hotpath_report(&mut recorder, None, REPORT.as_bytes()).unwrap();
        assert!(record_hotpath_report(&mut recorder, None, REPORT.as_bytes()).is_err());
    }
}
