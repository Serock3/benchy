use std::{fs, path::Path, process::Command};

use serde_json::{Value, json};

fn exporter(result_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_benchy-cli"));
    command.args(["export-bencher", "--result-dir"]);
    command.arg(result_dir);
    command
}

#[test]
fn exports_result_files_to_stdout_or_a_separate_file() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let stdout = exporter(&fixtures).output().unwrap();
    assert!(stdout.status.success(), "{:?}", stdout);
    let metrics: Value = serde_json::from_slice(&stdout.stdout).unwrap();
    assert_eq!(
        metrics["gotatun/gotatun-throughput/cpu.gotatun.up"]["percent"]["value"],
        json!(282.1)
    );
    assert_eq!(
        metrics["gotatun/gotatun-throughput/throughput.receiver"]["gigabits-per-second"]["value"],
        json!(2.73)
    );
    assert_eq!(metrics.as_object().unwrap().len(), 2);

    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("bencher.json");
    let result = exporter(&fixtures)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result);
    assert!(result.stdout.is_empty());
    assert_eq!(fs::read(output).unwrap(), stdout.stdout);
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
