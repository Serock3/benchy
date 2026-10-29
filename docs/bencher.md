# Bencher export and publishing

## Convert one workflow attempt

From the Benchy checkout:

```console
cargo run --locked --package benchy-cli -- export-bencher \
  --result-dir /path/to/downloaded/raw-results \
  --output /path/to/bencher.json
```

Omit `--output` to print JSON to stdout. Keep the output outside the input directory. The
converter reads the directory's top-level `.json` files in sorted order and requires schema v1.
It rejects failed results, empty inputs or measurement sets, non-finite values, duplicate metric
identities, and files from different repositories, revisions, branches, or workflow attempts.
Validation errors do not produce a partial export or truncate an existing output file.

Each Benchy measurement becomes a Bencher benchmark named
`<repository>/<benchmark-name>/<measurement-id>`. Display labels are excluded from the identity
so changing a label does not split the historical series. Throughput is converted from raw bits/s
to decimal Gb/s by dividing by 1,000,000,000. CPU percentages stay unchanged and may exceed 100
(100% represents one logical CPU). The raw result schema and values remain unchanged.

For example:

```json
{
  "gotatun/gotatun-throughput/cpu.gotatun.up": {
    "percent": { "value": 282.1 }
  },
  "gotatun/gotatun-throughput/throughput.receiver": {
    "gigabits-per-second": { "value": 2.73 }
  }
}
```

These custom measures keep the units explicit. Bencher's built-in `throughput` is operations/s
and `latency` is nanoseconds, so neither is used for our measurements.

| Benchy unit | Bencher measure slug | Bencher measure units |
| --- | --- | --- |
| `bits_per_second` | `gigabits-per-second` | `Gb/s` |
| `percent` | `percent` | `%` |
| `seconds` | `seconds` | `s` |
| `count` | `count` | `count` |

The initial test reports used the `bits-per-second` measure with `bits/s` units. New reports use
`gigabits-per-second`; the older measure and its values are left intact. Relabelling those older
values as Gb/s would make them incorrect by a factor of one billion.

The publisher creates any missing measures used by the report and verifies that existing ones
have the listed units. A mismatch fails publication without changing historical definitions.
BMF contains metric values and measure identifiers, but no unit metadata, so these definitions
are configured explicitly before uploading.

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

`scripts/publish-bencher.sh` verifies the measures and submits the converted metrics. PR reports
use Bencher branch `pr/<number>` and the PR head SHA, with the target branch and base SHA as the
start point. The start point is reset for each run so a rebased PR does not keep using an older
baseline. Pushes to `main` establish baseline history using the same testbed. Statistical
regression thresholds are not configured yet; PR checks initially report successful measurement
and publication, rather than certifying the absence of a performance regression.

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
