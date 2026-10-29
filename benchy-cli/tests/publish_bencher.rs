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
        fs::write(&metrics, r#"{
          "gotatun/gotatun-throughput/throughput.receiver": {"gigabits-per-second": {"value": 2.671151827}},
          "gotatun/gotatun-throughput/cpu.gotatun.up": {"percent": {"value": 277.7}}
        }"#).unwrap();
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/publish-bencher.sh");
        let mut command = Command::new("bash");
        command
            .arg(script)
            .arg(metrics)
            .arg(temp.path().join("report.json"))
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("BENCHER_API_KEY", "test-key-never-log")
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
    assert!(calls.contains("--slug gigabits-per-second --units Gb/s"));
    assert!(calls.contains("--slug percent --units %"));
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
fn creates_gigabit_measure_without_relabelling_existing_bit_measure() {
    let mut publisher = Publisher::new("legacy-units");
    let output = publisher.command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let calls = publisher.read("calls.log");
    assert!(calls.contains("--slug gigabits-per-second --units Gb/s"));
    assert!(!calls.contains("--slug bits-per-second"));
    assert!(!calls.contains("measure update"));
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
