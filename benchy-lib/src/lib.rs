use std::{borrow::Borrow, collections::BTreeMap, fmt};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const RESULT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SchemaVersion;

impl Serialize for SchemaVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(RESULT_SCHEMA_VERSION)
    }
}

impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = u32::deserialize(deserializer)?;
        if version == RESULT_SCHEMA_VERSION {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(format_args!(
                "unsupported benchmark result schema version {version}"
            )))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd, Eq, Ord, Hash)]
pub struct BenchmarkGroup {
    pub repository: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Benchmark {
    pub schema_version: SchemaVersion,
    #[serde(flatten)]
    pub group: BenchmarkGroup,
    pub commit: String,
    pub branch: String,
    pub commit_message: Option<String>,
    pub description: String,
    pub date: DateTime<Utc>,
    pub measurements: BTreeMap<MetricId, Measurement>,
    /// Named sub-benchmarks sharing the result's source identity and parameters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cases: BTreeMap<String, BTreeMap<MetricId, Measurement>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub status: BenchmarkStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MetricId(String);

impl MetricId {
    pub fn new(id: impl Into<String>) -> Result<Self, InvalidMetricId> {
        let id = id.into();
        if is_valid_metric_id(&id) {
            Ok(Self(id))
        } else {
            Err(InvalidMetricId(id))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MetricId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl Borrow<str> for MetricId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for MetricId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidMetricId(String);

impl fmt::Display for InvalidMetricId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid metric ID {:?}; expected lowercase dot-separated identifiers",
            self.0
        )
    }
}

impl std::error::Error for InvalidMetricId {}

fn is_valid_metric_id(id: &str) -> bool {
    id.split('.').all(|segment| {
        let mut bytes = segment.bytes();
        bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            && bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkStatus {
    Success,
    Failed,
}

impl BenchmarkStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Measurement {
    pub label: String,
    pub unit: Unit,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<Uncertainty>,
}

/// Estimated uncertainty of the measurement's mean, in the measurement's own unit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Uncertainty {
    pub method: UncertaintyMethod,
    pub sample_count: u32,
    pub standard_error: f64,
}

impl Uncertainty {
    pub fn is_valid(&self) -> bool {
        self.sample_count >= 2 && self.standard_error.is_finite() && self.standard_error >= 0.0
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyMethod {
    IndependentIntervals,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Unit {
    BitsPerSecond,
    Percent,
    Seconds,
    Nanoseconds,
    Count,
    Bytes,
}

impl Unit {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BitsPerSecond => "bits_per_second",
            Self::Percent => "percent",
            Self::Seconds => "seconds",
            Self::Nanoseconds => "nanoseconds",
            Self::Count => "count",
            Self::Bytes => "bytes",
        }
    }
}

pub mod iperf {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Output {
        pub end: End,
        #[serde(flatten)]
        pub interval_output: IntervalOutput,
    }

    /// Interval rates describe the endpoint emitting them, rather than both directions.
    /// `--get-server-output` with a JSON server supplies the other endpoint's intervals.
    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    pub struct IntervalOutput {
        #[serde(default)]
        pub intervals: Vec<Interval>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub server_output_json: Option<Box<IntervalOutput>>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Interval {
        pub sum: IntervalSum,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct IntervalSum {
        pub bits_per_second: f64,
        pub seconds: f64,
        #[serde(default)]
        pub omitted: bool,
        #[serde(default)]
        pub sender: Option<bool>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct End {
        pub sum_sent: Sum,
        pub sum_received: Sum,
        pub cpu_utilization_percent: CpuUtilization,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Sum {
        pub bits_per_second: f64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct CpuUtilization {
        pub host_total: f64,
        pub host_user: f64,
        pub host_system: f64,
        pub remote_total: f64,
        pub remote_user: f64,
        pub remote_system: f64,
    }

    /// iperf3 JSON output of a UDP test. Newer iperf3 versions report the sender and
    /// receiver summaries separately in `end.sum_sent` and `end.sum_received`; older
    /// versions only emit the ambiguous `end.sum`, disambiguated by its `sender` flag.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct UdpOutput {
        pub end: UdpEnd,
        #[serde(flatten)]
        pub interval_output: IntervalOutput,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct UdpEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub sum: Option<UdpSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub sum_sent: Option<UdpSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub sum_received: Option<UdpSummary>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct UdpSummary {
        pub bits_per_second: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub jitter_ms: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub lost_percent: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub sender: Option<bool>,
    }

    impl UdpEnd {
        /// The sender-side summary, if the test reported one.
        pub fn sent(&self) -> Option<&UdpSummary> {
            self.sum_sent.as_ref().or_else(|| {
                self.sum
                    .as_ref()
                    .filter(|summary| summary.sender == Some(true))
            })
        }

        /// The receiver-side summary, if the test reported one. This is the side that
        /// carries jitter and loss statistics.
        pub fn received(&self) -> Option<&UdpSummary> {
            self.sum_received.as_ref().or_else(|| {
                self.sum
                    .as_ref()
                    .filter(|summary| summary.sender == Some(false))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::{
        Benchmark, BenchmarkGroup, BenchmarkStatus, Measurement, MetricId, SchemaVersion, Unit,
        iperf,
    };

    #[test]
    fn schema_v1_shape_round_trips() {
        let benchmark = Benchmark {
            schema_version: SchemaVersion,
            group: BenchmarkGroup {
                repository: "gotatun".to_owned(),
                name: "gotatun-throughput".to_owned(),
            },
            commit: "abc123".to_owned(),
            branch: "main".to_owned(),
            commit_message: Some("Measure throughput".to_owned()),
            description: "GotaTun throughput".to_owned(),
            date: Utc.with_ymd_and_hms(2026, 9, 25, 12, 0, 0).unwrap(),
            measurements: BTreeMap::from([(
                MetricId::new("throughput.sender").unwrap(),
                Measurement {
                    label: "Sender throughput".to_owned(),
                    unit: Unit::BitsPerSecond,
                    value: 2_500_000_000.0,
                    uncertainty: None,
                },
            )]),
            cases: BTreeMap::new(),
            run_id: Some("123456".to_owned()),
            status: BenchmarkStatus::Success,
            error: None,
            parameters: BTreeMap::from([("duration_seconds".to_owned(), "30".to_owned())]),
            environment: BTreeMap::from([("RUNNER_NAME".to_owned(), "benchy-alice".to_owned())]),
        };

        let json = serde_json::to_value(&benchmark).unwrap();
        assert_eq!(
            json,
            json!({
                "schema_version": 1,
                "repository": "gotatun",
                "name": "gotatun-throughput",
                "commit": "abc123",
                "branch": "main",
                "commit_message": "Measure throughput",
                "description": "GotaTun throughput",
                "date": "2026-09-25T12:00:00Z",
                "measurements": {
                    "throughput.sender": {
                        "label": "Sender throughput",
                        "unit": "bits_per_second",
                        "value": 2_500_000_000.0
                    }
                },
                "run_id": "123456",
                "status": "success",
                "parameters": { "duration_seconds": "30" },
                "environment": { "RUNNER_NAME": "benchy-alice" }
            })
        );
        assert_eq!(
            serde_json::from_value::<Benchmark>(json).unwrap(),
            benchmark
        );
    }

    #[test]
    fn unknown_schema_versions_are_rejected() {
        let result = serde_json::from_value::<Benchmark>(json!({ "schema_version": 2 }));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported benchmark result schema version 2")
        );
    }

    #[test]
    fn uncertainty_is_an_optional_v1_measurement_field() {
        let original = json!({"label": "Throughput", "unit": "bits_per_second", "value": 2e9});
        let legacy: Measurement = serde_json::from_value(original.clone()).unwrap();
        assert!(legacy.uncertainty.is_none());
        assert_eq!(serde_json::to_value(legacy).unwrap(), original);
        let mut with_uncertainty = original;
        with_uncertainty["uncertainty"] = json!({
            "method": "independent_intervals", "sample_count": 30, "standard_error": 5e6
        });
        let measurement: Measurement = serde_json::from_value(with_uncertainty.clone()).unwrap();
        assert!(measurement.uncertainty.as_ref().unwrap().is_valid());
        assert_eq!(serde_json::to_value(measurement).unwrap(), with_uncertainty);
        with_uncertainty["uncertainty"]["method"] = json!("unknown");
        assert!(serde_json::from_value::<Measurement>(with_uncertainty).is_err());
    }

    #[test]
    fn metric_ids_are_stable_machine_identifiers() {
        for valid in [
            "throughput.sender",
            "cpu.gotatun.up",
            "latency.p95_seconds",
            "reconnect.count-v2",
        ] {
            assert!(MetricId::new(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "UP CPU", "cpu..up", ".cpu", "cpu.UP", "9cpu.up"] {
            assert!(MetricId::new(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn udp_iperf_output_parses_separate_sender_and_receiver_summaries() {
        let json = json!({
            "end": {
                "sum_sent": {
                    "bits_per_second": 900_000_000.0,
                    "jitter_ms": 0.0,
                    "lost_packets": 0,
                    "packets": 7500,
                    "lost_percent": 0.0,
                    "sender": true
                },
                "sum_received": {
                    "bits_per_second": 810_000_000.0,
                    "jitter_ms": 0.007,
                    "lost_packets": 75,
                    "packets": 7425,
                    "lost_percent": 1.0,
                    "sender": false
                }
            }
        });

        let output = serde_json::from_value::<iperf::UdpOutput>(json).unwrap();
        assert_eq!(output.end.sent().unwrap().bits_per_second, 900_000_000.0);
        let received = output.end.received().unwrap();
        assert_eq!(received.bits_per_second, 810_000_000.0);
        assert_eq!(received.lost_percent, Some(1.0));
        assert_eq!(received.jitter_ms, Some(0.007));
    }

    #[test]
    fn udp_iperf_output_parses_legacy_ambiguous_summary() {
        let json = json!({
            "end": {
                "sum": {
                    "bits_per_second": 810_000_000.0,
                    "jitter_ms": 0.007,
                    "lost_percent": 1.0,
                    "sender": false
                }
            }
        });

        let output = serde_json::from_value::<iperf::UdpOutput>(json).unwrap();
        assert!(output.end.sent().is_none());
        let received = output.end.received().unwrap();
        assert_eq!(received.bits_per_second, 810_000_000.0);
        assert_eq!(received.lost_percent, Some(1.0));
    }

    #[test]
    fn udp_iperf_output_without_sender_flag_has_no_side_summaries() {
        let json = json!({
            "end": {
                "sum": { "bits_per_second": 810_000_000.0 }
            }
        });

        let output = serde_json::from_value::<iperf::UdpOutput>(json).unwrap();
        assert!(output.end.sent().is_none());
        assert!(output.end.received().is_none());
    }

    #[test]
    fn udp_iperf_output_prefers_separate_summaries_over_ambiguous_one() {
        let json = json!({
            "end": {
                "sum": {
                    "bits_per_second": 700_000_000.0,
                    "sender": true
                },
                "sum_sent": {
                    "bits_per_second": 900_000_000.0,
                    "sender": true
                },
                "sum_received": {
                    "bits_per_second": 810_000_000.0,
                    "sender": false
                }
            }
        });

        let output = serde_json::from_value::<iperf::UdpOutput>(json).unwrap();
        assert_eq!(output.end.sent().unwrap().bits_per_second, 900_000_000.0);
        assert_eq!(
            output.end.received().unwrap().bits_per_second,
            810_000_000.0
        );
    }
}
