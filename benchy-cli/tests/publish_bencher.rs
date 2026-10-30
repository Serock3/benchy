#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Publisher {
    temp: tempfile::TempDir,
    command: Command,
}

impl Publisher {
    fn new(mode: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let mock = bin.join("bencher");
        fs::write(&mock, include_str!("fixtures/bencher-mock.sh")).unwrap();
        fs::set_permissions(&mock, fs::Permissions::from_mode(0o700)).unwrap();
        let metrics = temp.path().join("metrics.json");
        fs::write(
            &metrics,
            r#"{
          "gotatun-throughput": {
            "throughput-receiver": {"value": 2.671151827},
            "cpu-gotatun-up": {"value": 277.7}
          }
        }"#,
        )
        .unwrap();
        fs::write(temp.path().join("measures.json"), r#"{
          "throughput-receiver": {"name": "Receiver throughput (Gb/s)", "units": "Gb/s"},
          "cpu-gotatun-up": {"name": "UP GotaTun CPU (logical CPU %)", "units": "% of one logical CPU"}
        }"#).unwrap();
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/publish-bencher.sh");
        let mut command = Command::new("bash");
        command
            .arg(script)
            .arg(metrics)
            .arg(temp.path().join("report.json"))
            .arg(temp.path().join("measures.json"))
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("BENCHER_API_KEY", "test-key-never-log")
            .env_remove("BENCHY_GITHUB_TOKEN")
            .env_remove("BENCHY_PR_NUMBER")
            .env_remove("BENCHY_SOURCE_SHA")
            .env_remove("BENCHY_SOURCE_BRANCH")
            .env_remove("BENCHY_BASE_BRANCH")
            .env_remove("BENCHY_BASE_SHA")
            .env("BENCHER_PROJECT", "gotatun-test")
            .env("BENCHER_TESTBED", "alice-bob-v1")
            .env("GITHUB_SHA", "9e54a1629126077d2486e26e414c3aadf6a201a2")
            .env("GITHUB_REF_NAME", "feature/benchmark")
            .env("GITHUB_STEP_SUMMARY", temp.path().join("summary.md"))
            .env("MOCK_MODE", mode)
            .env("MOCK_LOG", temp.path().join("calls.log"));
        Self { temp, command }
    }

    fn read(&self, filename: &str) -> String {
        fs::read_to_string(self.temp.path().join(filename)).unwrap_or_default()
    }
}

#[test]
fn reuses_existing_units_and_links_the_published_report() {
    let mut publisher = Publisher::new("existing");
    let output = publisher.command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let calls = publisher.read("calls.log");
    assert!(!calls.contains("measure create"));
    assert!(
        calls
            .contains("--branch feature/benchmark --hash 9e54a1629126077d2486e26e414c3aadf6a201a2")
    );
    assert!(calls.contains("--testbed alice-bob-v1"));
    assert!(!calls.contains("test-key-never-log"));
    assert!(
        publisher
            .read("summary.md")
            .contains("https://bencher.dev/console/projects/gotatun-test/reports/report-id")
    );
    assert!(publisher.read("report.json").contains("report-id"));
}

#[test]
fn creates_only_missing_measures_used_by_the_report() {
    let mut publisher = Publisher::new("missing");
    let output = publisher.command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let calls = publisher.read("calls.log");
    assert!(
        calls.contains("--name Receiver throughput (Gb/s) --slug throughput-receiver --units Gb/s")
    );
    assert!(calls.contains(
        "--name UP GotaTun CPU (logical CPU %) --slug cpu-gotatun-up --units % of one logical CPU"
    ));
    assert!(!calls.contains("--slug seconds"));
    assert!(!calls.contains("--slug count"));
}

#[test]
fn unit_mismatches_and_api_failures_do_not_publish_a_report() {
    for mode in ["wrong-units", "wrong-throughput-units", "api-failure"] {
        let mut publisher = Publisher::new(mode);
        let output = publisher.command.output().unwrap();
        assert!(!output.status.success(), "{mode}");
        assert!(!publisher.read("calls.log").contains("run --project"));
        assert!(publisher.read("summary.md").is_empty());
    }
}

#[test]
fn creates_grouped_measures_without_relabelling_legacy_measures() {
    let mut publisher = Publisher::new("legacy-units");
    let output = publisher.command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let calls = publisher.read("calls.log");
    assert!(calls.contains("--slug throughput-receiver --units Gb/s"));
    assert!(calls.contains("--slug cpu-gotatun-up --units % of one logical CPU"));
    assert!(!calls.contains("--slug bits-per-second"));
    assert!(!calls.contains("--slug percent"));
    assert!(!calls.contains("measure update"));
}

#[test]
fn definitions_must_match_the_published_measures() {
    let mut publisher = Publisher::new("existing");
    fs::write(publisher.temp.path().join("measures.json"), "{}").unwrap();
    let output = publisher.command.output().unwrap();
    assert!(!output.status.success());
    assert!(publisher.read("calls.log").is_empty());
}

#[test]
fn thresholds_detect_throughput_drops_and_target_cpu_increases_only() {
    let mut publisher = Publisher::new("missing");
    let mut metrics: serde_json::Value =
        serde_json::from_str(&publisher.read("metrics.json")).unwrap();
    let mut measures: serde_json::Value =
        serde_json::from_str(&publisher.read("measures.json")).unwrap();
    for (slug, name, units) in [
        (
            "cpu-iperf-down",
            "DOWN iperf CPU (logical CPU %)",
            "% of one logical CPU",
        ),
        ("udp-lost-percent", "UDP packet loss (%)", "%"),
        ("udp-jitter", "UDP jitter (s)", "s"),
    ] {
        metrics["gotatun-throughput"][slug] = serde_json::json!({"value": 1.0});
        measures[slug] = serde_json::json!({"name": name, "units": units});
    }
    fs::write(
        publisher.temp.path().join("metrics.json"),
        serde_json::to_vec(&metrics).unwrap(),
    )
    .unwrap();
    fs::write(
        publisher.temp.path().join("measures.json"),
        serde_json::to_vec(&measures).unwrap(),
    )
    .unwrap();
    let output = publisher.command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let calls = publisher.read("calls.log");
    assert!(calls.contains("--threshold-measure throughput-receiver --threshold-test percentage --threshold-min-sample-size 1 --threshold-max-sample-size 30 --threshold-lower-boundary 0.05 --threshold-upper-boundary _"));
    assert!(calls.contains("--threshold-measure cpu-gotatun-up --threshold-test percentage --threshold-min-sample-size 1 --threshold-max-sample-size 30 --threshold-lower-boundary _ --threshold-upper-boundary 0.10"));
    assert!(!calls.contains("--threshold-measure cpu-iperf"));
    assert!(!calls.contains("--threshold-measure udp-"));
    assert_eq!(calls.matches("--threshold-measure").count(), 2);
}

#[test]
fn missing_credentials_fail_before_calling_bencher() {
    let mut publisher = Publisher::new("existing");
    publisher.command.env_remove("BENCHER_API_KEY");
    let output = publisher.command.output().unwrap();
    assert!(!output.status.success());
    assert!(publisher.read("calls.log").is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("BENCHER_API_KEY"));
}

#[test]
fn failed_upload_does_not_add_a_success_link() {
    let mut publisher = Publisher::new("upload-failure");
    let output = publisher.command.output().unwrap();
    assert!(!output.status.success());
    assert!(publisher.read("calls.log").contains("run --project"));
    assert!(publisher.read("summary.md").is_empty());
}

#[test]
fn pr_reporting_uses_tested_head_and_base_instead_of_publisher_commit() {
    let mut publisher = Publisher::new("existing");
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let base = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    publisher
        .command
        .env("BENCHY_SOURCE_SHA", sha)
        .env("BENCHY_SOURCE_BRANCH", "pr/7")
        .env("BENCHY_PR_NUMBER", "7")
        .env("BENCHY_BASE_BRANCH", "main")
        .env("BENCHY_BASE_SHA", base)
        .env("BENCHY_GITHUB_TOKEN", "github-test-token")
        .env("MOCK_EXPECT_SHA", sha);
    let output = publisher.command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let calls = publisher.read("calls.log");
    assert!(calls.contains(&format!("--branch pr/7 --hash {sha}")));
    assert!(calls.contains("--github-actions github-test-token"));
    assert!(calls.contains("--ci-only-thresholds"));
    assert!(calls.contains("--ci-number 7 --start-point main"));
    assert!(calls.contains(&format!("--start-point-hash {base} --start-point-reset")));
}

#[test]
fn pr_reporting_requires_github_credentials_before_publishing() {
    let mut publisher = Publisher::new("existing");
    publisher.command.env("BENCHY_PR_NUMBER", "7");
    let output = publisher.command.output().unwrap();
    assert!(!output.status.success());
    assert!(publisher.read("calls.log").is_empty());
}

#[test]
fn shared_directional_measures_keep_target_cpu_thresholds_only() {
    let mut publisher = Publisher::new("missing");
    fs::write(publisher.temp.path().join("metrics.json"), r#"{
        "gotatun-download": {"throughput-network":{"value":2.7},"cpu-gotatun":{"value":375},"cpu-iperf":{"value":75}},
        "gotatun-upload": {"throughput-network":{"value":2.8},"cpu-gotatun":{"value":280},"cpu-iperf":{"value":6}}
    }"#).unwrap();
    fs::write(
        publisher.temp.path().join("measures.json"),
        r#"{
        "throughput-network":{"name":"Throughput (Gb/s)","units":"Gb/s"},
        "cpu-gotatun":{"name":"GotaTun CPU (logical CPU %)","units":"% of one logical CPU"},
        "cpu-iperf":{"name":"iperf CPU (logical CPU %)","units":"% of one logical CPU"}
    }"#,
    )
    .unwrap();
    let output = publisher.command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let calls = publisher.read("calls.log");
    assert!(calls.contains("--threshold-measure throughput-network"));
    assert!(calls.contains("--threshold-measure cpu-gotatun"));
    assert!(!calls.contains("--threshold-measure cpu-iperf"));
    assert_eq!(calls.matches("--threshold-measure").count(), 2);
}
