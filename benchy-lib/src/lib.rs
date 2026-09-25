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
}

impl Measurement {
    pub fn format_f64(&self) -> impl Fn(f64) -> String {
        match self.unit {
            Unit::BitsPerSecond => format_bits_per_second,
            Unit::Percent => format_percent,
            Unit::Seconds => format_seconds,
            Unit::Count => format_count,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Unit {
    BitsPerSecond,
    Percent,
    Seconds,
    Count,
}

impl Unit {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BitsPerSecond => "bits_per_second",
            Self::Percent => "percent",
            Self::Seconds => "seconds",
            Self::Count => "count",
        }
    }
}

fn format_percent(value: f64) -> String {
    format!("{value:.2}%")
}

fn format_seconds(value: f64) -> String {
    format!("{value:.2} s")
}

fn format_count(value: f64) -> String {
    format!("{value:.0}")
}

pub mod iperf {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Output {
        pub end: End,
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
}

fn format_bits_per_second(bps: f64) -> String {
    let bps = bps.abs();

    let one_gbps = 1_000_000_000.0;
    let one_mbps = 1_000_000.0;
    let one_kbps = 1_000.0;

    let (suffix, value) = if bps >= one_gbps {
        ("Gbps", bps / one_gbps)
    } else if bps >= one_mbps {
        ("Mbps", bps / one_mbps)
    } else {
        ("Kbps", bps / one_kbps)
    };

    format!("{value:.2} {suffix}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::{
        Benchmark, BenchmarkGroup, BenchmarkStatus, Measurement, MetricId, SchemaVersion, Unit,
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
                },
            )]),
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
}
