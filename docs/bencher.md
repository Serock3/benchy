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
so changing a label does not split the historical series. Values remain in their original units,
including CPU percentages above 100 (100% represents one logical core).

For example:

```json
{
  "gotatun/gotatun-throughput/cpu.gotatun.up": {
    "percent": { "value": 282.1 }
  },
  "gotatun/gotatun-throughput/throughput.receiver": {
    "bits-per-second": { "value": 2730000000.0 }
  }
}
```

These custom measures keep the units explicit. Bencher's built-in `throughput` is operations/s
and `latency` is nanoseconds, so neither is used for our measurements.

| Benchy unit | Bencher measure slug | Bencher measure units |
| --- | --- | --- |
| `bits_per_second` | `bits-per-second` | `bits/s` |
| `percent` | `percent` | `%` |
| `seconds` | `seconds` | `s` |
| `count` | `count` | `count` |

The publisher creates any missing measures used by the report and verifies that existing ones
have the listed units. A mismatch fails publication without changing historical definitions.
BMF contains metric values and measure identifiers, but no unit metadata, so these definitions
are configured explicitly before uploading.

The BMF file contains only metrics. Preserve the raw JSON artifact for source metadata,
parameters, environment, labels, and failures. Pass the tested revision and branch explicitly to
`bencher run`; checking out Benchy to build the converter must not cause results to be attributed
to Benchy's revision. Parameter changes that affect comparability require a different benchmark
identity or Bencher testbed.

## Publishing job

The reusable workflow in `.github/workflows/run-benchmarks.yml` contains two jobs:

```yaml
jobs:
  benchmark:
    # Runs benchmarks and uploads the raw result artifact.
    runs-on: ${{ fromJSON(inputs.runner-labels) }}

  publish:
    needs: benchmark
    if: ${{ inputs.bencher-project != '' }}
    runs-on: ubuntu-latest
    permissions:
      contents: read
    # Converts and publishes results using the product's project key.
```

`ubuntu-latest` requests a fresh GitHub-hosted machine. `needs: benchmark` waits until Alice's
job succeeds; failures still retain their raw artifact and summary, without entering the baseline.
The benchmark job passes its exact artifact ID and Benchy commit to the publisher. The publisher
downloads that artifact and checks out the matching framework revision, verifies the raw results'
commit, branch, run ID, and original attempt, then builds the converter. Using the original
artifact ID and attempt also allows retrying a failed publishing job while retaining the earlier
successful benchmark job. Product source is not checked out or executed by the publisher.

Each product caller supplies the optional `bencher-project` input and explicitly forwards the
named `BENCHER_API_KEY` secret:

```yaml
jobs:
  benchmark:
    uses: Serock3/benchy/.github/workflows/run-benchmarks.yml@main
    with:
      benchmarks: ${{ inputs.benchmarks || 'default' }}
      manifest-path: benchmarks/Cargo.toml
      bencher-project: ${{ vars.BENCHER_PROJECT }}
    secrets:
      BENCHER_API_KEY: ${{ secrets.BENCHER_API_KEY }}
```

Only the publishing step references the key in its environment. An empty project input disables
publication for repositories still using artifacts. A configured project without a forwarded key
fails publication with an explicit error. Set the Actions repository variable `BENCHER_PROJECT`
to `gotatun-test` for the GotaTun pilot; `mullvad-test` is a different project and rejects that key.
Each product repository can have different values under the same variable and secret names.

After downloading and converting the artifact, `scripts/publish-bencher.sh` configures the
required measures and runs the equivalent of:

```console
bencher run --project "$BENCHER_PROJECT" \
  --branch "$GITHUB_REF_NAME" --hash "$GITHUB_SHA" \
  --testbed "$BENCHER_TESTBED" --adapter json --file bencher-results/metrics.json
```

The tested commit and branch are taken from the calling workflow context, checked against the
raw results before publishing, and passed explicitly so the framework checkout cannot change
their attribution. Use the optional `bencher-testbed` input (default `alice-bob-v1`) to distinguish
hardware or configuration changes that affect comparability.

The Bencher CLI returns a JSON report, saved as `bencher-results/report.json`, and its URL is
added to the workflow summary. The converted metrics and report are uploaded as
`bencher-results-<run-id>-<publishing-attempt>` even when publication fails. Raw results remain in
their original artifact. Authentication, unit, and service errors fail the publishing job without
rerunning or discarding the benchmark. The official setup action installs the latest CLI for
Bencher Cloud compatibility; the action itself is pinned to a commit.

Neither GitHub checks/PR comments nor regression thresholds are enabled yet. They can follow
after an end-to-end Actions pilot; reporting features will require additional job permissions.

## References

- [Bencher Metric Format](https://bencher.dev/docs/reference/bencher-metric-format/)
- [Project measures API](https://bencher.dev/docs/api/projects/measures/)
- [Bencher run](https://bencher.dev/docs/explanation/bencher-run/)
- [Sharing artifacts between jobs](https://docs.github.com/en/actions/tutorials/store-and-share-data)
- [Passing secrets to reusable workflows](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows)
