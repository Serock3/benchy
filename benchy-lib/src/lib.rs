use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd, Eq, Ord, Hash)]
pub struct BenchmarkGroup {
    pub repository: String,
    pub name: String,
}

/// ```json
/// {
/// "name": "network throughput",
/// "values": { "up": { "bps": 1000.0 } }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Benchmark {
    #[serde(flatten)]
    pub group: BenchmarkGroup,
    pub commit: String,
    pub branch: String,
    pub commit_message: Option<String>,
    pub description: String,
    pub date: DateTime<Utc>,
    pub values: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default)]
    pub status: BenchmarkStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkStatus {
    #[default]
    Success,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Value {
    #[serde(rename = "bps")]
    Bps(f64),
    #[serde(rename = "percent")]
    Percent(f64),
    #[serde(rename = "seconds")]
    Seconds(f64),
    #[serde(rename = "count")]
    Count(f64),
}

impl Value {
    pub fn as_f64(&self) -> f64 {
        match *self {
            Value::Bps(bps) => bps,
            Value::Percent(percent) => percent,
            Value::Seconds(seconds) => seconds,
            Value::Count(count) => count,
        }
    }

    pub fn format_f64(&self) -> impl Fn(f64) -> String {
        match self {
            Value::Bps(..) => format_bits_per_second,
            Value::Percent(..) => format_percent,
            Value::Seconds(..) => format_seconds,
            Value::Count(..) => format_count,
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
