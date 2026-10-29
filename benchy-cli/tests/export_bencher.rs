use std::{fs, path::Path, process::Command};

use serde_json::{Value, json};

fn exporter(result_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_benchy-cli"));
    command.args(["export-bencher", "--result-dir"]);
    command.arg(result_dir);
    command
}

fn repeated_results(directory: &Path) {
    fs::create_dir_all(directory).unwrap();
    fs::write(
        directory.join("_samples.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1, "iterations": 3, "benchmarks": ["gotatun-throughput"]
        }))
        .unwrap(),
    )
    .unwrap();
    for index in 1..=3 {
        let sample = directory.join(format!("sample-{index:03}"));
        fs::create_dir(&sample).unwrap();
        let mut raw: Value = serde_json::from_str(include_str!("fixtures/gotatun.json")).unwrap();
        raw["environment"]["BENCHY_SAMPLE_INDEX"] = json!(index.to_string());
        raw["environment"]["BENCHY_SAMPLE_COUNT"] = json!("3");
        raw["measurements"]["throughput.receiver"]["value"] = json!(index as f64 * 1e9);
        raw["measurements"]["throughput.receiver"]["uncertainty"] = json!({
            "method": "independent_intervals", "sample_count": 30, "standard_error": 100_000_000.0
        });
        fs::write(
            sample.join("gotatun-throughput.json"),
            serde_json::to_vec(&raw).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn exports_complete_repeated_runs_with_bounds_and_preserves_raw_samples() {
    let temp = tempfile::tempdir().unwrap();
    repeated_results(temp.path());
    let source = temp.path().join("sample-001/gotatun-throughput.json");
    let before = fs::read(&source).unwrap();
    let output = exporter(temp.path()).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let metrics: Value = serde_json::from_slice(&output.stdout).unwrap();
    let throughput = &metrics["gotatun-throughput"]["throughput-receiver"];
    assert_eq!(throughput["value"], json!(2.0));
    assert!(
        (throughput["upper_value"].as_f64().unwrap() - (2.0 + 0.1 / 3.0_f64.sqrt())).abs() < 1e-12
    );
    assert_eq!(
        metrics["gotatun-throughput"]["cpu-gotatun-up"],
        json!({"value": 282.1})
    );
    assert_eq!(fs::read(source).unwrap(), before);
}

#[test]
fn rejects_incomplete_mixed_or_misidentified_samples_before_exporting() {
    for case in [
        "missing",
        "extra",
        "flat",
        "index",
        "source",
        "manifest",
        "benchmark",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let raw = temp.path().join("raw");
        repeated_results(&raw);
        let sample = raw.join("sample-003/gotatun-throughput.json");
        match case {
            "missing" => fs::remove_dir_all(raw.join("sample-003")).unwrap(),
            "extra" => fs::create_dir(raw.join("sample-004")).unwrap(),
            "flat" => {
                fs::copy(&sample, raw.join("flat.json")).unwrap();
            }
            "manifest" => fs::remove_file(raw.join("_samples.json")).unwrap(),
            _ => {
                let mut value: Value = serde_json::from_slice(&fs::read(&sample).unwrap()).unwrap();
                match case {
                    "index" => value["environment"]["BENCHY_SAMPLE_INDEX"] = json!("1"),
                    "source" => value["commit"] = json!("unrelated"),
                    "benchmark" => value["name"] = json!("unexpected-benchmark"),
                    _ => unreachable!(),
                }
                fs::write(sample, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        let destination = temp.path().join("metrics.json");
        fs::write(&destination, "previous export").unwrap();
        let output = exporter(&raw)
            .arg("--output")
            .arg(&destination)
            .output()
            .unwrap();
        assert!(!output.status.success(), "case {case}: {output:?}");
        assert!(output.stdout.is_empty(), "case {case}");
        assert_eq!(fs::read_to_string(destination).unwrap(), "previous export");
    }
}

#[test]
fn runs_complete_samples_sequentially_and_does_not_publish_a_partial_batch() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("benchmark");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(
        workspace.join("Cargo.toml"),
        r#"
[workspace]
[package]
name = "sample-benchmark"
version = "0.1.0"
edition = "2024"
[package.metadata.benchy]
id = "gotatun-throughput"
default = true
"#,
    )
    .unwrap();
    fs::write(
        workspace.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"sample-benchmark\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let mut fixture: Value = serde_json::from_str(include_str!("fixtures/gotatun.json")).unwrap();
    fixture["environment"]["BENCHY_SAMPLE_INDEX"] = json!("__INDEX__");
    fixture["environment"]["BENCHY_SAMPLE_COUNT"] = json!("__COUNT__");
    let fixture = serde_json::to_string(&fixture).unwrap();
    fs::write(
        workspace.join("src/main.rs"),
        format!(
            r#"
fn main() {{
    let index = std::env::var("BENCHY_SAMPLE_INDEX").unwrap();
    if std::env::var("FAIL_SAMPLE_INDEX").ok().as_ref() == Some(&index) {{
        std::process::exit(1);
    }}
    let count = std::env::var("BENCHY_SAMPLE_COUNT").unwrap();
    let raw = {fixture:?}.replace("__INDEX__", &index).replace("__COUNT__", &count);
    let directory = std::env::var("BENCHY_RESULT_DIR").unwrap();
    std::fs::write(std::path::Path::new(&directory).join("gotatun-throughput.json"), raw).unwrap();
}}
"#
        ),
    )
    .unwrap();
    let run = |directory: &Path, fail: &str| {
        Command::new(env!("CARGO_BIN_EXE_benchy-cli"))
            .args(["run", "--manifest-path"])
            .arg(workspace.join("Cargo.toml"))
            .args(["--iterations", "3", "--result-dir"])
            .arg(directory)
            .env("CARGO_TARGET_DIR", temp.path().join("target"))
            .env("FAIL_SAMPLE_INDEX", fail)
            .output()
            .unwrap()
    };
    let results = temp.path().join("success");
    let output = run(&results, "");
    assert!(output.status.success(), "{output:?}");
    let output = exporter(&results).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let metrics: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        metrics["gotatun-throughput"]["throughput-receiver"],
        json!({"value": 2.73})
    );
    // An existing batch cannot be silently reused, even after a successful run.
    assert!(!run(&results, "").status.success());
    let partial = temp.path().join("partial");
    assert!(!run(&partial, "2").status.success());
    assert!(partial.join("sample-001/gotatun-throughput.json").exists());
    assert!(!partial.join("sample-003").exists());
    assert!(!exporter(&partial).output().unwrap().status.success());
}

#[test]
fn exports_result_files_to_stdout_or_a_separate_file() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let stdout = exporter(&fixtures).output().unwrap();
    assert!(stdout.status.success(), "{:?}", stdout);
    let metrics: Value = serde_json::from_slice(&stdout.stdout).unwrap();
    assert_eq!(
        metrics["gotatun-throughput"]["cpu-gotatun-up"]["value"],
        json!(282.1)
    );
    assert_eq!(
        metrics["gotatun-throughput"]["throughput-receiver"]["value"],
        json!(2.73)
    );
    assert_eq!(metrics.as_object().unwrap().len(), 1);
    assert_eq!(metrics["gotatun-throughput"].as_object().unwrap().len(), 2);

    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("bencher.json");
    let measures = temp.path().join("measures.json");
    let result = exporter(&fixtures)
        .arg("--output")
        .arg(&output)
        .arg("--measures-output")
        .arg(&measures)
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result);
    assert!(result.stdout.is_empty());
    assert_eq!(fs::read(output).unwrap(), stdout.stdout);
    let definitions: Value = serde_json::from_slice(&fs::read(measures).unwrap()).unwrap();
    assert_eq!(
        definitions,
        json!({
            "cpu-gotatun-up": {"name": "UP GotaTun CPU (logical CPU %)", "units": "% of one logical CPU"},
            "throughput-receiver": {"name": "Receiver throughput (Gb/s)", "units": "Gb/s"}
        })
    );
}

#[test]
fn invalid_inputs_do_not_overwrite_outputs_or_source_documents() {
    let temp = tempfile::tempdir().unwrap();
    let result_dir = temp.path().join("raw");
    fs::create_dir(&result_dir).unwrap();
    let output = temp.path().join("bencher.json");
    fs::write(&output, "previous export").unwrap();
    let input = result_dir.join("gotatun.json");
    let fixture = include_str!("fixtures/gotatun.json");

    for version in [None, Some(2)] {
        let mut invalid: Value = serde_json::from_str(fixture).unwrap();
        match version {
            None => {
                invalid.as_object_mut().unwrap().remove("schema_version");
            }
            Some(version) => invalid["schema_version"] = json!(version),
        }
        fs::write(&input, serde_json::to_vec(&invalid).unwrap()).unwrap();
        let result = exporter(&result_dir)
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr).contains("invalid benchmark result"));
        assert_eq!(fs::read_to_string(&output).unwrap(), "previous export");
    }

    fs::write(&input, fixture).unwrap();
    let result = exporter(&result_dir)
        .arg("--output")
        .arg(&input)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("outside the result directory"));
    assert_eq!(fs::read_to_string(input).unwrap(), fixture);
}

#[test]
fn publication_context_is_checked_before_writing_an_export() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let temp = tempfile::tempdir().unwrap();
    let context_path = temp.path().join("context.json");
    let output_path = temp.path().join("metrics.json");
    let mut context = json!({
        "repository": "owner/gotatun", "run_id": 123456, "run_attempt": 1,
        "artifact_id": 99, "sha": "abc123", "branch": "main", "pull_request": null
    });
    fs::write(&context_path, serde_json::to_vec(&context).unwrap()).unwrap();
    let output = exporter(&fixtures)
        .arg("--publication-context")
        .arg(&context_path)
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let previous = fs::read(&output_path).unwrap();
    context["sha"] = json!("unrelated-commit");
    fs::write(&context_path, serde_json::to_vec(&context).unwrap()).unwrap();
    let output = exporter(&fixtures)
        .arg("--publication-context")
        .arg(&context_path)
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not match"));
    assert_eq!(fs::read(output_path).unwrap(), previous);
}
