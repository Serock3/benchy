use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use benchy_lib::Benchmark;
use benchy_store::Store;
use cargo_metadata::{MetadataCommand, Package};
use clap::{Parser, Subcommand};
use serde::Deserialize;

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
    },
    /// Add result JSON documents to a persistent SQLite database.
    Ingest {
        #[arg(long)]
        database: PathBuf,
        #[arg(long, default_value = "benchmark-results")]
        result_dir: PathBuf,
    },
    /// Show the newest runs stored in a SQLite database.
    Inspect {
        #[arg(long)]
        database: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Deserialize)]
struct BenchyMetadata {
    id: String,
    #[serde(default)]
    default: bool,
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
        } => run(manifest_path, &benchmarks, result_dir),
        Commands::Ingest {
            database,
            result_dir,
        } => ingest(&database, &result_dir),
        Commands::Inspect {
            database,
            limit,
            json,
        } => inspect(&database, limit, json),
    }
}

fn inspect(database: &Path, limit: usize, json: bool) -> Result<()> {
    if limit == 0 {
        bail!("--limit must be greater than zero");
    }
    let store = Store::open_read_only(database)?;
    let runs = store.latest_runs(limit)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
    } else if runs.is_empty() {
        println!("No benchmark runs stored in {}", database.display());
    } else {
        for run in runs {
            let commit = run.commit.chars().take(12).collect::<String>();
            println!(
                "#{}  {}  {:7}  {}/{}  {}  {}  {} measurement(s)",
                run.id,
                run.date.to_rfc3339(),
                run.status.as_str(),
                run.group.repository,
                run.group.name,
                run.branch,
                commit,
                run.measurement_count,
            );
        }
    }
    Ok(())
}

fn ingest(database: &Path, result_dir: &Path) -> Result<()> {
    let mut paths = fs::read_dir(result_dir)
        .with_context(|| format!("failed to read {}", result_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();

    let benchmarks = paths
        .iter()
        .map(|path| {
            let bytes = fs::read(path)
                .with_context(|| format!("failed to read result {}", path.display()))?;
            serde_json::from_slice::<Benchmark>(&bytes)
                .with_context(|| format!("invalid benchmark result {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut store = Store::open(database)?;
    let report = store.ingest(&benchmarks)?;
    println!(
        "Stored {} benchmark result(s) in {} ({} inserted, {} updated)",
        report.total(),
        database.display(),
        report.inserted,
        report.updated
    );
    Ok(())
}

fn run(manifest_path: PathBuf, selection: &str, result_dir: PathBuf) -> Result<()> {
    let packages = benchmark_packages(&manifest_path)?;
    let requested = parse_selection(selection);
    let run_all = requested.contains("all");
    let run_default = requested.contains("default");
    let mut selected = Vec::new();
    let mut found = BTreeSet::new();

    for (package, metadata) in packages {
        if run_all || (run_default && metadata.default) || requested.contains(metadata.id.as_str())
        {
            found.insert(metadata.id.clone());
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

    let mut failed = BTreeSet::new();
    for (package, metadata) in selected {
        println!("::group::benchmark {}", metadata.id);
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
            .env("BENCHY_RESULT_DIR", &result_dir)
            .status()
            .with_context(|| format!("failed to launch benchmark {}", metadata.id))?;
        println!("::endgroup::");
        let result_path = result_dir.join(format!("{}.json", metadata.id));
        if status.success() && !result_path.is_file() {
            eprintln!(
                "benchmark {} succeeded without writing {}",
                metadata.id,
                result_path.display()
            );
            failed.insert(metadata.id.clone());
        }
        if !status.success() {
            failed.insert(metadata.id);
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
