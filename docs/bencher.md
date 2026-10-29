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
converter reads raw `.json` files in sorted order and requires schema v1.
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
GotaTun CPU, and UP and DOWN iperf CPU. They belong to one benchmark; none are discarded.
These custom measures keep the units explicit. Bencher's built-in `throughput` is
operations/s and `latency` is nanoseconds, so neither is used for our measurements.

| Benchy unit | Measure name suffix | Bencher measure units |
| --- | --- | --- |
| `bits_per_second` | `(Gb/s)` | `Gb/s` |
| `percent` with a `cpu.*` metric ID | `(logical CPU %)` | `% of one logical CPU` |
| Other `percent`, e.g. UDP packet loss | `(%)` | `%` |
| `seconds` | `(s)` | `s` |
| `nanoseconds` | `(ns)` | `nanoseconds` |
| `count` | `(count)` | `count` |
| `bytes` | `(B)` | `bytes` |

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

## Throughput uncertainty

The default is one complete 30-second transfer. Sender and receiver throughput may carry an
`uncertainty` object with `method: independent_intervals`, `sample_count`, and `standard_error`,
in the raw measurement's unit. The estimate is `s / sqrt(n)`, where `s` is the sample standard
deviation of interval rates (N−1 denominator). It estimates uncertainty in the transfer's average;
it is not the interval SD itself or an observed SD across complete runs.

Benchmarks explicitly request one-second iperf reports, a JSON server, and `--get-server-output`.
Sender intervals supply sender uncertainty; receiver intervals supply receiver uncertainty.
Omitted warmup intervals and short final fragments (less than 0.9 seconds) are excluded.
Missing intervals or fewer than two usable intervals produce no uncertainty, rather than zero.
The whole-transfer throughput value stays as reported in iperf's final summary.

The exporter publishes `lower_value = value - standard_error` and
`upper_value = value + standard_error`, converting both throughput and its uncertainty to Gb/s.
These are one-standard-error bounds, not confidence intervals. The model assumes approximately
independent, similarly sized intervals and negligible additional variation in conditions between
runs. It does not correct for autocorrelation. Repeated GotaTun trials showed about 1.35% total
relative SD in throughput over ten consecutive 30-second runs; that is short-term evidence for
this pragmatic assumption, not proof that between-run effects vanish.

CPU is sampled only over the whole transfer, so it has no estimated uncertainty. UDP jitter,
packet loss, and profiling values also stay without bounds. Thresholds still compare central
values; adding bounds does not turn them into significance tests.

## Repeated complete runs

To measure run-to-run variation, pass `--iterations 10` to `benchy-cli run`, or set the
reusable workflow's `iterations` number input. The default is 1; the allowed range is 1–100.
Each sample starts a fresh benchmark process, including its normal endpoint setup, complete
transfer, and cleanup. Compiled code is reused. With multiple benchmarks, each round runs all
selected benchmarks sequentially before the next round. The result directory must be empty.
A failed benchmark stops further rounds and the partial batch cannot be published.

The directory contains `_samples.json`, declaring the iteration count and selected benchmark IDs.
For more than one iteration, raw results are stored in `sample-001/`, `sample-002/`, etc.
Each raw document remains schema v1 and records `BENCHY_SAMPLE_INDEX` and `BENCHY_SAMPLE_COUNT`
in its environment. With one iteration, raw files remain at the top level. Legacy single-run
artifacts without a manifest can still be exported. All raw samples appear in the workflow
summary and artifact.

The exporter requires every declared sample and benchmark, identical parameters and measure sets,
and the same tested revision and workflow attempt. It publishes the arithmetic mean as `value`.
If every sample supplies uncertainty, the aggregate standard error is
`sqrt(sum(standard_error_i^2)) / iterations`, assuming independent runs and the same negligible
between-run-effects model. If any sample lacks uncertainty, the aggregate has no bounds. CPU
therefore stays without bounds regardless of iteration count. Individual samples remain available
for separately analysing empirical run-to-run variation. Earlier repeated-run reports used
empirical across-run SD; their already-published historical bounds remain unchanged.

## PR runs and approval

GotaTun's `Benchmarks` workflow runs on pushes to `main`; the app's runs nightly on `main` at
01:17 UTC. Both run for PRs targeting `main` and support manual dispatch with a benchmark
selection. They call `run-benchmarks.yml` with read-only repository
permissions and no secrets. PRs check out the head repository and exact head SHA. The workflow
sets `BENCHY_SOURCE_SHA` and `BENCHY_SOURCE_BRANCH=pr/<number>` so the raw results identify that
commit rather than GitHub's synthetic merge commit. Other runs use the selected commit and branch;
scheduled runs use the latest commit on the default branch. The publisher accepts `schedule`
events with the same source-run and artifact validation as push and manual runs.

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

The publisher configures percentage-based regression thresholds for the measures in each report:

| Measurement | Alert condition relative to the historical mean |
| --- | --- |
| Sender and receiver throughput | More than 5% lower |
| Target process CPU (`cpu.*`, excluding `cpu.iperf.*`) | More than 10% higher |
| iperf CPU, UDP loss, jitter, and other diagnostics | Recorded without automatic thresholds |

Each threshold uses up to 30 prior samples and requires at least one. A first baseline report
therefore records values without evaluating a regression. These initial tolerances should be
revisited after observing run-to-run noise; they are relative changes, not confidence levels or
CPU percentage-point changes. See [Bencher's percentage test](https://bencher.dev/docs/explanation/thresholds/#percentage).
The publishing workflow itself succeeds when results are uploaded successfully; Bencher's
separate GitHub check fails when a regression alert is generated, and its PR comment shows alerts.
`--ci-only-thresholds` keeps informational measures out of the PR comment and avoids missing-
threshold warnings there. All measures remain stored in the full Bencher report.

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
