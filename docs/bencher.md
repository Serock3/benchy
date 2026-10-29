# Bencher export and publishing

## Convert one workflow attempt

From the Benchy checkout:

```console
cargo run --locked --package benchy-cli -- export-bencher \
  --result-dir /path/to/downloaded/raw-results \
  --output /path/to/bencher.json \
  --measures-output /path/to/measures.json
```

Omit `--output` to print BMF JSON to stdout. `--measures-output` writes the custom measure
definitions needed by the publisher; it can be omitted for a standalone BMF export. Keep both
outputs outside the input directory and use different files. The
converter reads the directory's top-level `.json` files in sorted order and requires schema v1.
It rejects failed results, empty inputs or measurement sets, non-finite values, duplicate metric
identities, and files from different repositories, revisions, branches, or workflow attempts.
Validation errors do not produce a partial export or truncate an existing output file.

Each Benchy benchmark becomes one Bencher benchmark named `<benchmark-name>`, with all its
measurements grouped underneath. The repository is already identified by the Bencher project.
Each metric ID becomes a measure slug by replacing dots and underscores with hyphens, e.g.
`cpu.gotatun.down` becomes `cpu-gotatun-down`. Colliding slugs and conflicting definitions for a
shared measure are rejected. Display labels are excluded from identities, so changing a label
does not split the historical series. Throughput is converted from raw bits/s
to decimal Gb/s by dividing by 1,000,000,000. CPU percentages stay unchanged and may exceed 100
(100% represents one logical CPU). The raw result schema and values remain unchanged.

For example:

```json
{
  "gotatun-throughput": {
    "cpu-gotatun-up": { "value": 282.1 },
    "throughput-receiver": { "value": 2.73 }
  }
}
```

The corresponding measure definitions are:

```json
{
  "cpu-gotatun-up": {
    "name": "UP GotaTun CPU (logical CPU %)",
    "units": "% of one logical CPU"
  },
  "throughput-receiver": {
    "name": "Receiver throughput (Gb/s)",
    "units": "Gb/s"
  }
}
```

The complete GotaTun benchmark has six measures: sender and receiver throughput, UP and DOWN
GotaTun CPU, and UP and DOWN iperf CPU. They belong to one benchmark; none are averaged or
discarded. These custom measures keep the units explicit. Bencher's built-in `throughput` is
operations/s and `latency` is nanoseconds, so neither is used for our measurements.

| Benchy unit | Measure name suffix | Bencher measure units |
| --- | --- | --- |
| `bits_per_second` | `(Gb/s)` | `Gb/s` |
| `percent` with a `cpu.*` metric ID | `(logical CPU %)` | `% of one logical CPU` |
| Other `percent`, e.g. UDP packet loss | `(%)` | `%` |
| `seconds` | `(s)` | `s` |
| `count` | `(count)` | `count` |

Names use the raw measurement's label followed by the suffix above; the earlier CPU suffix
`(% of one logical CPU)` is replaced rather than duplicated. Bencher limits measure names and
slugs to 64 characters. Labels and units for a shared metric ID must agree across benchmarks.

Earlier reports used a separate benchmark per metric, with generic `bits-per-second`,
`gigabits-per-second`, and `percent` measures. Those reports remain intact. The grouped format
starts new series, so publish a main-branch baseline before comparing PR results in this format.
Raw artifacts from old runs can be converted again with the new exporter without rerunning the
benchmarks. Do not relabel the original bits/s values as Gb/s.

The publisher creates any missing measures used by the report and verifies that existing ones
have the listed units. A mismatch fails publication without changing historical definitions.
An existing display-name mismatch produces a diagnostic; project keys cannot rename measures,
so apply later name changes in Bencher with a user key. BMF contains metric values and measure
identifiers, but no display names or unit metadata, so definitions from `--measures-output` are
configured explicitly before uploading. The definitions must exactly match the measures in BMF.

The BMF file contains only metrics. Preserve the raw JSON artifact for source metadata,
parameters, environment, labels, and failures. Pass the tested revision and branch explicitly to
`bencher run`; checking out Benchy to build the converter must not cause results to be attributed
to Benchy's revision. Parameter changes that affect comparability require a different benchmark
identity or Bencher testbed.

## PR runs and approval

The product's `Benchmarks` workflow runs on pushes to `main`, PRs targeting `main`, and manual
dispatch with a benchmark selection. It calls `run-benchmarks.yml` with read-only repository
permissions and no secrets. PRs check out the head repository and exact head SHA. The workflow
sets `BENCHY_SOURCE_SHA` and `BENCHY_SOURCE_BRANCH=pr/<number>` so the raw results identify that
commit rather than GitHub's synthetic merge commit. Other runs use the selected commit and branch.

Before enabling this trigger, configure **Settings → Actions → General → Approval for running
fork pull request workflows from contributors → Require approval for all external contributors**.
Internal PRs run automatically. External contributors' workflows wait for a maintainer's
**Approve workflows to run** action on the PR page, including contributors with earlier merged
PRs. This setting is repository-wide, so it also affects the repository's other workflows.

Approval authorizes running the reviewed code on the benchmark hosts. It does not sandbox build
scripts or benchmark binaries, or isolate the hosts' existing SSH keys and sudo privileges.
GitHub withholds repository secrets and write tokens from fork PR runs even after approval.

## Trusted publishing workflow

Each product has a second workflow on its default branch, for example
`.github/workflows/publish-benchmarks.yml`:

```yaml
name: Publish benchmarks
on:
  workflow_run:
    workflows: [Benchmarks]
    types: [completed]
permissions: {}
jobs:
  publish:
    if: ${{ github.event.workflow_run.conclusion == 'success' }}
    permissions:
      actions: read
      contents: read
      checks: write
      pull-requests: write
    uses: Serock3/benchy/.github/workflows/publish-benchmarks.yml@main
    with:
      bencher-project: ${{ vars.BENCHER_PROJECT }}
    secrets:
      BENCHER_API_KEY: ${{ secrets.BENCHER_API_KEY }}
```

This workflow runs in the base repository's trusted context. It calls the shared publisher on
`ubuntu-latest`, checks out Benchy from its trusted `main`, and does not check out product code or
use a framework revision supplied by the benchmark artifact. Only its final publishing step gets
the Bencher key and GitHub reporting token. The official Bencher setup action is pinned to a
commit; it installs the latest CLI for Bencher Cloud compatibility.

The product sets its own `BENCHER_API_KEY` secret and `BENCHER_PROJECT` variable. Use
`gotatun-test` for the current GotaTun fork; a key for that project cannot publish to `mullvad-test`.
An empty project disables publication. A configured project without a key fails explicitly. The
optional `bencher-testbed` input defaults to `alice-bob-v1` and should change for hardware or
benchmark configuration changes that affect comparability.

`benchy-cli prepare-publication` resolves the exact completed source run attempt using GitHub's
API. It verifies the repository, workflow path/ID, successful conclusion, source SHA, and event,
then selects the unexpired artifact named `benchmark-results-<source-run-id>-<source-attempt>`.
PR metadata also comes from GitHub's API. If the run has no associated PR numbers (which can
happen for forks), the commit association API supplies candidates. The publisher requires one
open PR whose head SHA, source repository, branch, and base repository match the run. Updated or
closed PRs are skipped; ambiguous associations fail. An artifact cannot choose the PR, checkout
ref, Bencher branch, or baseline.

The command writes `bencher-results/context.json` and step outputs for artifact download. Every
raw result's commit, branch, source run ID, and source attempt must match this trusted context
before `export-bencher --publication-context ...` produces metrics. Downloaded content is parsed
as JSON only. No commands, workflow files, or scripts from the artifact are executed.

## Reporting and baseline

`scripts/publish-bencher.sh METRICS_JSON REPORT_JSON MEASURES_JSON` verifies the measures and
submits the converted metrics. PR reports
use Bencher branch `pr/<number>` and the PR head SHA, with the target branch and base SHA as the
start point. The start point is reset for each run so a rebased PR does not keep using an older
baseline. Pushes to `main` establish baseline history using the same testbed.

The publishing script declares percentage-based thresholds for both measures: throughput alerts
when it drops more than 10% below the historical mean, and CPU usage alerts when it rises more
than 25% above it. Calibrated against the observed run-to-run spread (about 3% for throughput
and up to 11% for the smallest CPU metric), they are intended to catch real regressions without
firing on noise. `bencher run` creates these thresholds for each run's branch and testbed, with a
minimum sample size of one while history is sparse. Alerts currently appear in the check and PR
comment; the publishing workflow is not failed on alert, so a red check still requires human
judgement rather than certifying the absence of a performance regression.

The publisher passes `--github-actions` and the explicit `--ci-number` for PR comments. Bencher
also reads `GITHUB_SHA` to place GitHub checks; the script overrides it for the Bencher subprocess
with the validated source SHA because a `workflow_run` job's own SHA belongs to the default branch.
The report's values, check, and PR comment therefore refer to the same tested revision.

The JSON report is saved as `bencher-results/report.json`, and its URL is added to the workflow
summary. Context, converted metrics, and the report are retained as a diagnostics artifact named
with both source and publishing run IDs/attempts, even if an upload fails. Raw results remain in
the original benchmark artifact. A failed publishing workflow can be retried independently.

## Deployment

Set and verify the external-contributor approval policy first. Push the shared Benchy changes,
then the updated product workflows to the fork's default branch. Both workflows must be present
on that branch for `workflow_run` to work. Check a main-branch baseline run, an internal PR, and a
PR opened by an outside contributor to verify the approval button and reporting. Copying the
external branch into the base repository is not a test of the fork approval gate.

## References

- [Bencher Metric Format](https://bencher.dev/docs/reference/bencher-metric-format/)
- [Project measures API](https://bencher.dev/docs/api/projects/measures/)
- [Bencher run](https://bencher.dev/docs/explanation/bencher-run/)
- [Sharing artifacts between jobs](https://docs.github.com/en/actions/tutorials/store-and-share-data)
- [Passing secrets to reusable workflows](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows)
