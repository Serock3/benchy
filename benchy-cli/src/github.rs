//! Resolve publishing targets from GitHub-owned run/PR metadata. Artifacts provide values only.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use benchy_lib::Benchmark;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
struct Repository {
    full_name: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Run {
    id: u64,
    run_attempt: u64,
    workflow_id: u64,
    path: String,
    event: String,
    status: String,
    conclusion: Option<String>,
    head_sha: String,
    head_branch: Option<String>,
    head_repository: Repository,
    repository: Repository,
    #[serde(default)]
    pull_requests: Vec<PrNumber>,
}

#[derive(Clone, Debug, Deserialize)]
struct PrNumber {
    number: u64,
}

#[derive(Debug, Deserialize)]
struct Event {
    action: String,
    repository: Repository,
    workflow_run: Run,
}

#[derive(Debug, Deserialize)]
struct PullRequest {
    number: u64,
    state: String,
    head: Revision,
    base: Revision,
}

#[derive(Debug, Deserialize)]
struct Revision {
    sha: String,
    #[serde(rename = "ref")]
    branch: String,
    repo: Option<Repository>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    id: u64,
    name: String,
    expired: bool,
}

#[derive(Debug, Deserialize)]
struct Artifacts {
    artifacts: Vec<Artifact>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Publication {
    pub repository: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub artifact_id: u64,
    pub sha: String,
    pub branch: String,
    pub pull_request: Option<PullRequestContext>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PullRequestContext {
    pub number: u64,
    pub base_branch: String,
    pub base_sha: String,
}

impl Publication {
    pub fn verify(&self, benchmarks: &[Benchmark]) -> Result<()> {
        ensure!(!benchmarks.is_empty(), "no benchmark results to publish");
        for benchmark in benchmarks {
            ensure!(
                benchmark.commit == self.sha
                    && benchmark.branch == self.branch
                    && benchmark.run_id.as_deref() == Some(self.run_id.to_string().as_str())
                    && benchmark.environment.get("GITHUB_RUN_ATTEMPT")
                        == Some(&self.run_attempt.to_string()),
                "artifact result identity does not match the trusted source workflow run"
            );
        }
        Ok(())
    }

    fn outputs(&self) -> Result<String> {
        let mut outputs = format!(
            "publish=true\nartifact-id={}\nsource-run-id={}\nsource-run-attempt={}\nsource-sha={}\nsource-branch={}\n",
            self.artifact_id,
            self.run_id,
            self.run_attempt,
            self.sha,
            output_value(&self.branch)?
        );
        if let Some(pr) = &self.pull_request {
            outputs += &format!(
                "pr-number={}\nbase-branch={}\nbase-sha={}\n",
                pr.number,
                output_value(&pr.base_branch)?,
                pr.base_sha
            );
        }
        Ok(outputs)
    }
}

pub fn prepare(
    event: &Path,
    repository: &str,
    workflow_path: &str,
    output: &Path,
    github_output: &Path,
) -> Result<()> {
    let event: Event =
        serde_json::from_slice(&fs::read(event)?).context("expected a workflow_run event")?;
    let publication = resolve(&event, repository, workflow_path, &mut api)?;
    let outputs = match &publication {
        Some(context) => context.outputs()?,
        None => {
            eprintln!("Skipping publication: no open PR still points at the tested revision");
            "publish=false\n".to_owned()
        }
    };
    fs::write(output, serde_json::to_vec_pretty(&publication)?)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(github_output)?
        .write_all(outputs.as_bytes())?;
    Ok(())
}

fn api(endpoint: &str) -> Result<Value> {
    // The GH_TOKEN belongs to the trusted publisher. Never pass it as a command argument.
    let output = Command::new("gh")
        .args(["api", "--method", "GET", endpoint])
        .output()
        .context("failed to launch GitHub CLI")?;
    ensure!(
        output.status.success(),
        "GitHub API request failed for {endpoint}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("invalid GitHub API response")
}

fn resolve(
    event: &Event,
    repository: &str,
    workflow_path: &str,
    api: &mut impl FnMut(&str) -> Result<Value>,
) -> Result<Option<Publication>> {
    validate_repository(repository)?;
    ensure!(
        event.action == "completed" && event.repository.full_name == repository,
        "unexpected publishing event or repository"
    );
    let source = &event.workflow_run;
    ensure!(
        source.id > 0 && source.run_attempt > 0,
        "invalid source run identity"
    );
    let run: Run = serde_json::from_value(api(&format!(
        "repos/{repository}/actions/runs/{}/attempts/{}",
        source.id, source.run_attempt
    ))?)?;
    ensure!(
        run.id == source.id
            && run.run_attempt == source.run_attempt
            && run.workflow_id == source.workflow_id
            && run.path == workflow_path
            && run.repository.full_name == repository
            && run.head_sha == source.head_sha
            && run.event == source.event
            && run.head_repository.full_name == source.head_repository.full_name
            && run.status == "completed"
            && run.conclusion.as_deref() == Some("success"),
        "source workflow identity or successful conclusion could not be verified"
    );
    validate_sha(&run.head_sha)?;

    let (branch, pull_request) = match run.event.as_str() {
        "pull_request" => {
            // GitHub can omit run.pull_requests for fork PRs. Recover candidates using
            // GitHub's pull request head filter or the commit association API, but never
            // trust a PR number or branch from an artifact.
            let candidates: Vec<PullRequest> = if run.pull_requests.is_empty() {
                pull_request_candidates(&run, repository, api)?
            } else {
                run.pull_requests
                    .iter()
                    .map(|pr| {
                        ensure!(pr.number > 0, "invalid associated PR number");
                        serde_json::from_value(api(&format!(
                            "repos/{repository}/pulls/{}",
                            pr.number
                        ))?)
                        .map_err(Into::into)
                    })
                    .collect::<Result<_>>()?
            };
            let mut candidates = candidates.into_iter().filter(|pr| {
                pr.state == "open"
                    && pr.head.sha == run.head_sha
                    && pr
                        .head
                        .repo
                        .as_ref()
                        .is_some_and(|repo| repo.full_name == run.head_repository.full_name)
                    && Some(&pr.head.branch) == run.head_branch.as_ref()
                    && pr
                        .base
                        .repo
                        .as_ref()
                        .is_some_and(|repo| repo.full_name == repository)
            });
            let Some(pr) = candidates.next() else {
                return Ok(None);
            };
            ensure!(
                candidates.next().is_none(),
                "ambiguous PR association for source run"
            );
            ensure!(pr.number > 0, "invalid PR number");
            validate_sha(&pr.base.sha)?;
            output_value(&pr.base.branch)?;
            (
                format!("pr/{}", pr.number),
                Some(PullRequestContext {
                    number: pr.number,
                    base_branch: pr.base.branch,
                    base_sha: pr.base.sha,
                }),
            )
        }
        "push" | "workflow_dispatch" | "schedule" => {
            ensure!(
                run.head_repository.full_name == repository,
                "non-PR run comes from a different repository"
            );
            let branch = run.head_branch.context("source run has no branch")?;
            output_value(&branch)?;
            (branch, None)
        }
        _ => bail!("unsupported source event: {}", run.event),
    };
    let expected_name = format!("benchmark-results-{}-{}", run.id, run.run_attempt);
    let artifacts: Artifacts = serde_json::from_value(api(&format!(
        "repos/{repository}/actions/runs/{}/artifacts?per_page=100",
        run.id
    ))?)?;
    let mut artifacts = artifacts
        .artifacts
        .into_iter()
        .filter(|artifact| artifact.name == expected_name && !artifact.expired);
    let artifact = artifacts
        .next()
        .context("source attempt has no unexpired benchmark artifact")?;
    ensure!(
        artifact.id > 0 && artifacts.next().is_none(),
        "invalid or ambiguous benchmark artifact"
    );
    Ok(Some(Publication {
        repository: repository.into(),
        run_id: run.id,
        run_attempt: run.run_attempt,
        artifact_id: artifact.id,
        sha: run.head_sha,
        branch,
        pull_request,
    }))
}

/// GitHub omits `run.pull_requests` for fork PRs. Recover candidates using GitHub's pull
/// request head filter, which also covers fork heads; the commit association API indexes
/// fork commits on the fork instead of the product repository, so it only serves as a
/// fallback when the head filter has no results.
fn pull_request_candidates(
    run: &Run,
    repository: &str,
    api: &mut impl FnMut(&str) -> Result<Value>,
) -> Result<Vec<PullRequest>> {
    if let Some(branch) = run.head_branch.as_deref() {
        let owner = run
            .head_repository
            .full_name
            .split('/')
            .next()
            .filter(|owner| !owner.is_empty())
            .context("source run has no head repository owner")?;
        let listed: Vec<PullRequest> = serde_json::from_value(api(&format!(
            "repos/{repository}/pulls?head={}:{}&per_page=100",
            percent_encode(owner),
            percent_encode(branch),
        ))?)
        .context("invalid pull request head filter response")?;
        if !listed.is_empty() {
            return Ok(listed);
        }
    }
    serde_json::from_value(api(&format!(
        "repos/{repository}/commits/{}/pulls?per_page=100",
        run.head_sha
    ))?)
    .context("invalid pull request association response")
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn validate_repository(repository: &str) -> Result<()> {
    let parts: Vec<_> = repository.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|part| !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))),
        "invalid repository name"
    );
    Ok(())
}

fn validate_sha(sha: &str) -> Result<()> {
    ensure!(
        sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid commit SHA"
    );
    Ok(())
}

fn output_value(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty() && !value.contains(['\r', '\n']),
        "invalid workflow output value"
    );
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REPO: &str = "owner/product";
    const WORKFLOW: &str = ".github/workflows/benchmarks.yml";
    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BASE_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn run(event: &str, fork: bool) -> Value {
        json!({
            "id": 11, "run_attempt": 2, "workflow_id": 42, "path": WORKFLOW,
            "event": event, "status": "completed", "conclusion": "success", "head_sha": SHA,
            "head_branch": "feature", "repository": {"full_name": REPO},
            "head_repository": {"full_name": if fork {"contributor/product"} else {REPO}},
            "pull_requests": if fork {json!([])} else {json!([{"number": 7}])}
        })
    }

    fn pr(fork: bool) -> Value {
        json!({
            "number": 7, "state": "open",
            "head": {"sha": SHA, "ref": "feature", "repo": {"full_name": if fork {"contributor/product"} else {REPO}}},
            "base": {"sha": BASE_SHA, "ref": "main", "repo": {"full_name": REPO}}
        })
    }

    fn artifacts() -> Value {
        json!({"artifacts": [
            {"id": 31, "name": "benchmark-results-11-1", "expired": false},
            {"id": 32, "name": "unrelated-artifact", "expired": false},
            {"id": 33, "name": "benchmark-results-11-2", "expired": false}
        ]})
    }

    fn event(run: &Value) -> Event {
        serde_json::from_value(
            json!({"action": "completed", "repository": {"full_name": REPO}, "workflow_run": run}),
        )
        .unwrap()
    }

    fn resolve_mock(
        event: &Event,
        run: Value,
        prs: Vec<Value>,
        artifacts: Value,
    ) -> Result<Option<Publication>> {
        let head_owner = run["head_repository"]["full_name"]
            .as_str()
            .and_then(|name| name.split('/').next())
            .unwrap_or("contributor");
        let head_filter = format!("repos/{REPO}/pulls?head={head_owner}:feature&per_page=100");
        resolve(event, REPO, WORKFLOW, &mut |endpoint| match endpoint {
            "repos/owner/product/actions/runs/11/attempts/2" => Ok(run.clone()),
            "repos/owner/product/pulls/7" => Ok(prs[0].clone()),
            path if path == head_filter => Ok(json!(prs)),
            path if path == format!("repos/{REPO}/commits/{SHA}/pulls?per_page=100") => {
                Ok(json!(prs))
            }
            "repos/owner/product/actions/runs/11/artifacts?per_page=100" => Ok(artifacts.clone()),
            _ => bail!("unexpected API request: {endpoint}"),
        })
    }

    #[test]
    fn resolves_non_pr_runs_and_exact_attempt_artifact_without_a_pr() {
        for source_event in ["push", "workflow_dispatch", "schedule"] {
            let branch = if source_event == "schedule" {
                "main"
            } else {
                "feature"
            };
            let mut source_run = run(source_event, false);
            source_run["head_branch"] = json!(branch);
            let context = resolve_mock(&event(&source_run), source_run, vec![], artifacts())
                .unwrap()
                .unwrap();
            assert_eq!(context.sha, SHA);
            assert_eq!(context.branch, branch);
            assert_eq!(context.artifact_id, 33);
            assert!(context.pull_request.is_none());

            let foreign = run(source_event, true);
            assert!(resolve_mock(&event(&foreign), foreign, vec![], artifacts()).is_err());
        }
    }

    #[test]
    fn resolves_internal_and_fork_prs_using_trusted_api_metadata() {
        for fork in [false, true] {
            let run = run("pull_request", fork);
            let context = resolve_mock(&event(&run), run, vec![pr(fork)], artifacts())
                .unwrap()
                .unwrap();
            assert_eq!(context.branch, "pr/7");
            assert_eq!(context.sha, SHA);
            let pr = context.pull_request.as_ref().unwrap();
            assert_eq!(pr.number, 7);
            assert_eq!(pr.base_branch, "main");
            assert_eq!(pr.base_sha, BASE_SHA);
            assert!(context.outputs().unwrap().contains("pr-number=7\n"));
        }
    }

    #[test]
    fn prefers_the_head_filter_over_commit_associations_for_fork_prs() {
        let run = run("pull_request", true);
        let context = resolve(
            &event(&run),
            REPO,
            WORKFLOW,
            &mut |endpoint| match endpoint {
                "repos/owner/product/actions/runs/11/attempts/2" => Ok(run.clone()),
                "repos/owner/product/pulls?head=contributor:feature&per_page=100" => {
                    Ok(json!([pr(true)]))
                }
                "repos/owner/product/actions/runs/11/artifacts?per_page=100" => Ok(artifacts()),
                _ => bail!("unexpected API request: {endpoint}"),
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(context.branch, "pr/7");
        assert_eq!(context.pull_request.as_ref().unwrap().number, 7);
    }

    #[test]
    fn resolves_fork_prs_via_commit_association_when_the_head_filter_is_empty() {
        let run = run("pull_request", true);
        let context = resolve(
            &event(&run),
            REPO,
            WORKFLOW,
            &mut |endpoint| match endpoint {
                "repos/owner/product/actions/runs/11/attempts/2" => Ok(run.clone()),
                "repos/owner/product/pulls?head=contributor:feature&per_page=100" => Ok(json!([])),
                path if path == format!("repos/{REPO}/commits/{SHA}/pulls?per_page=100") => {
                    Ok(json!([pr(true)]))
                }
                "repos/owner/product/actions/runs/11/artifacts?per_page=100" => Ok(artifacts()),
                _ => bail!("unexpected API request: {endpoint}"),
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(context.branch, "pr/7");
        assert_eq!(context.pull_request.as_ref().unwrap().number, 7);
    }

    #[test]
    fn skips_updated_closed_deleted_fork_and_unrelated_prs() {
        let run = run("pull_request", true);
        let mut updated = pr(true);
        updated["head"]["sha"] = json!(BASE_SHA);
        let mut closed = pr(true);
        closed["state"] = json!("closed");
        let mut deleted = pr(true);
        deleted["head"]["repo"] = Value::Null;
        let mut unrelated = pr(true);
        unrelated["base"]["repo"]["full_name"] = json!("other/repo");
        for pr in [updated, closed, deleted, unrelated] {
            assert!(
                resolve_mock(&event(&run), run.clone(), vec![pr], Value::Null)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn rejects_ambiguous_prs() {
        let run = run("pull_request", true);
        let mut other = pr(true);
        other["number"] = json!(8);
        assert!(resolve_mock(&event(&run), run, vec![pr(true), other], artifacts()).is_err());
    }

    #[test]
    fn rejects_spoofed_failed_or_unsupported_source_runs() {
        let run = run("workflow_dispatch", false);
        for (key, value) in [
            ("path", json!(".github/workflows/attacker.yml")),
            ("conclusion", json!("failure")),
            ("status", json!("in_progress")),
            ("workflow_id", json!(43)),
            ("head_sha", json!(BASE_SHA)),
            ("run_attempt", json!(3)),
        ] {
            let mut invalid = run.clone();
            invalid[key] = value;
            assert!(
                resolve_mock(&event(&run), invalid, vec![], artifacts()).is_err(),
                "{key}"
            );
        }
        let mut unsupported = run.clone();
        unsupported["event"] = json!("pull_request_target");
        assert!(resolve_mock(&event(&unsupported), unsupported, vec![], artifacts()).is_err());
        let mut foreign = run;
        foreign["head_repository"]["full_name"] = json!("attacker/product");
        assert!(resolve_mock(&event(&foreign), foreign, vec![], artifacts()).is_err());
    }

    #[test]
    fn rejects_wrong_attempt_expired_missing_and_duplicate_artifacts() {
        let run = run("push", false);
        let mut old = artifacts();
        old["artifacts"].as_array_mut().unwrap().pop();
        let mut expired = artifacts();
        expired["artifacts"][2]["expired"] = json!(true);
        let mut duplicate = artifacts();
        let artifact = duplicate["artifacts"][2].clone();
        duplicate["artifacts"]
            .as_array_mut()
            .unwrap()
            .push(artifact);
        for artifacts in [json!({"artifacts": []}), old, expired, duplicate] {
            assert!(resolve_mock(&event(&run), run.clone(), vec![], artifacts).is_err());
        }
    }

    #[test]
    fn verifies_every_result_identity_before_export() {
        let run = run("workflow_dispatch", false);
        let context = resolve_mock(&event(&run), run, vec![], artifacts())
            .unwrap()
            .unwrap();
        let mut benchmark: Benchmark =
            serde_json::from_str(include_str!("../tests/fixtures/gotatun.json")).unwrap();
        benchmark.commit = SHA.into();
        benchmark.branch = "feature".into();
        benchmark.run_id = Some("11".into());
        benchmark
            .environment
            .insert("GITHUB_RUN_ATTEMPT".into(), "2".into());
        assert!(context.verify(std::slice::from_ref(&benchmark)).is_ok());
        assert!(context.verify(&[]).is_err());
        let mut wrong_commit = benchmark.clone();
        wrong_commit.commit = BASE_SHA.into();
        let mut wrong_branch = benchmark.clone();
        wrong_branch.branch = "main".into();
        let mut wrong_run = benchmark.clone();
        wrong_run.run_id = Some("12".into());
        let mut wrong_attempt = benchmark.clone();
        wrong_attempt
            .environment
            .insert("GITHUB_RUN_ATTEMPT".into(), "1".into());
        for wrong in [wrong_commit, wrong_branch, wrong_run, wrong_attempt] {
            assert!(context.verify(&[benchmark.clone(), wrong]).is_err());
        }
    }

    #[test]
    fn rejects_workflow_output_injection_and_invalid_commit_ids() {
        assert!(output_value("feature\npublish=true").is_err());
        assert!(output_value("feature\rmain").is_err());
        assert!(validate_repository("owner/repo?malicious=true").is_err());
        assert!(validate_sha("not-a-commit").is_err());
    }

    #[test]
    fn branch_names_are_percent_encoded_for_the_head_filter() {
        assert_eq!(percent_encode("feature"), "feature");
        assert_eq!(percent_encode("feat/x"), "feat/x");
        assert_eq!(percent_encode("a b&c+d"), "a%20b%26c%2Bd");
    }
}
