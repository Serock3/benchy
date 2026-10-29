use std::{collections::BTreeSet, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail, ensure};
use cargo_metadata::{MetadataCommand, Package};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

mod bencher;
mod github;

#[derive(Parser)]
#[command(version, about = "Run repository-local system benchmarks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    List {
        #[arg(long, default_value = "benchmarks/Cargo.toml")]
        manifest_path: PathBuf,
    },
    Run {
        #[arg(long, default_value = "benchmarks/Cargo.toml")]
        manifest_path: PathBuf,
        #[arg(long, default_value = "default")]
        benchmarks: String,
        #[arg(long, default_value = "benchmark-results")]
        result_dir: PathBuf,
        /// Number of complete runs per benchmark; bounds in Bencher use sample standard deviation.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=100))]
        iterations: u16,
    },
    /// Convert a successful workflow attempt's result files to Bencher Metric Format.
    ExportBencher {
        #[arg(long, default_value = "benchmark-results")]
        result_dir: PathBuf,
        /// Write to this file instead of stdout. Keep it outside the input directory.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Write custom measure names and units for the publisher, outside the input directory.
        #[arg(long)]
        measures_output: Option<PathBuf>,
        /// Validate the results against trusted metadata resolved by prepare-publication.
        #[arg(long)]
        publication_context: Option<PathBuf>,
    },
    /// Resolve a completed benchmark run and its PR using GitHub's API, not artifact metadata.
    PreparePublication {
        #[arg(long)]
        event: PathBuf,
        #[arg(long)]
        repository: String,
        #[arg(long, default_value = ".github/workflows/benchmarks.yml")]
        workflow_path: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        github_output: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
struct BenchyMetadata {
    id: String,
    #[serde(default)]
    default: bool,
}

const SAMPLE_MANIFEST: &str = "_samples.json";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SampleManifest {
    schema_version: u32,
    iterations: u16,
    benchmarks: BTreeSet<String>,
}

impl SampleManifest {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported sample manifest version"
        );
        ensure!(
            (1..=100).contains(&self.iterations),
            "iterations must be between 1 and 100"
        );
        ensure!(
            !self.benchmarks.is_empty()
                && self.benchmarks.iter().all(|name| !name.trim().is_empty()),
            "sample manifest has no benchmark IDs"
        );
        Ok(())
    }

    fn directory(&self, root: &std::path::Path, index: u16) -> PathBuf {
        if self.iterations == 1 {
            root.to_owned()
        } else {
            root.join(format!("sample-{index:03}"))
        }
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::List { manifest_path } => {
            for (_, metadata) in benchmark_packages(&manifest_path)? {
                println!(
                    "{}{}",
                    metadata.id,
                    if metadata.default { " (default)" } else { "" }
                );
            }
            Ok(())
        }
        Commands::Run {
            manifest_path,
            benchmarks,
            result_dir,
            iterations,
        } => run(manifest_path, &benchmarks, result_dir, iterations),
        Commands::ExportBencher {
            result_dir,
            output,
            measures_output,
            publication_context,
        } => bencher::export(
            &result_dir,
            output.as_deref(),
            measures_output.as_deref(),
            publication_context.as_deref(),
        ),
        Commands::PreparePublication {
            event,
            repository,
            workflow_path,
            output,
            github_output,
        } => github::prepare(&event, &repository, &workflow_path, &output, &github_output),
    }
}

fn run(
    manifest_path: PathBuf,
    selection: &str,
    result_dir: PathBuf,
    iterations: u16,
) -> Result<()> {
    let packages = benchmark_packages(&manifest_path)?;
    let requested = parse_selection(selection);
    let run_all = requested.contains("all");
    let run_default = requested.contains("default");
    let mut selected = Vec::new();
    let mut found = BTreeSet::new();

    for (package, metadata) in packages {
        if run_all || (run_default && metadata.default) || requested.contains(metadata.id.as_str())
        {
            ensure!(
                found.insert(metadata.id.clone()),
                "duplicate benchmark ID: {}",
                metadata.id
            );
            selected.push((package, metadata));
        }
    }

    let unknown: Vec<_> = requested
        .difference(&found)
        .filter(|id| id.as_str() != "all" && id.as_str() != "default")
        .collect();
    if !unknown.is_empty() {
        bail!("unknown benchmarks: {unknown:?}");
    }
    if selected.is_empty() {
        bail!("benchmark selection '{selection}' matched no packages");
    }

    std::fs::create_dir_all(&result_dir)
        .with_context(|| format!("failed to create {}", result_dir.display()))?;
    let result_dir = result_dir.canonicalize()?;

    ensure!(
        std::fs::read_dir(&result_dir)?.next().is_none(),
        "--result-dir must be empty to avoid mixing old samples"
    );
    let manifest = SampleManifest {
        schema_version: 1,
        iterations,
        benchmarks: found,
    };
    manifest.validate()?;
    std::fs::write(
        result_dir.join(SAMPLE_MANIFEST),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    let mut failed = BTreeSet::new();
    for iteration in 1..=iterations {
        let sample_dir = manifest.directory(&result_dir, iteration);
        std::fs::create_dir_all(&sample_dir)?;
        for (package, metadata) in &selected {
            println!(
                "::group::benchmark {} (sample {iteration}/{iterations})",
                metadata.id
            );
            let status = Command::new("cargo")
                .args([
                    "run",
                    "--release",
                    "--locked",
                    "--config",
                    r#"target."cfg(unix)".runner="/usr/bin/env""#,
                    "--manifest-path",
                ])
                .arg(&manifest_path)
                .args(["--package", &package.name, "--"])
                .env("BENCHY_RESULT_DIR", &sample_dir)
                .env("BENCHY_SAMPLE_INDEX", iteration.to_string())
                .env("BENCHY_SAMPLE_COUNT", iterations.to_string())
                .status()
                .with_context(|| format!("failed to launch benchmark {}", metadata.id))?;
            println!("::endgroup::");
            let result_path = sample_dir.join(format!("{}.json", metadata.id));
            if status.success() && !result_path.is_file() {
                eprintln!(
                    "benchmark {} succeeded without writing {}",
                    metadata.id,
                    result_path.display()
                );
                failed.insert(format!("{} sample {iteration}", metadata.id));
            }
            if !status.success() {
                failed.insert(format!("{} sample {iteration}", metadata.id));
            }
        }
        if !failed.is_empty() {
            break;
        }
    }

    if failed.is_empty() {
        Ok(())
    } else {
        bail!(
            "benchmarks failed: {}",
            failed.into_iter().collect::<Vec<_>>().join(", ")
        )
    }
}

fn benchmark_packages(manifest_path: &PathBuf) -> Result<Vec<(Package, BenchyMetadata)>> {
    let metadata = MetadataCommand::new()
        .manifest_path(manifest_path)
        .no_deps()
        .exec()
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;

    metadata
        .workspace_packages()
        .into_iter()
        .filter_map(|package| {
            package.metadata.get("benchy").cloned().map(|value| {
                serde_json::from_value(value)
                    .map(|metadata| (package.clone(), metadata))
                    .with_context(|| {
                        format!("invalid benchy metadata for package {}", package.name)
                    })
            })
        })
        .collect()
}

fn parse_selection(selection: &str) -> BTreeSet<String> {
    selection
        .split([',', ' '])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_selection;

    #[test]
    fn parses_comma_and_space_delimited_selection() {
        let parsed = parse_selection("first, second third");
        assert_eq!(
            parsed.into_iter().collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
    }
}
