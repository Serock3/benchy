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

Create these measures with the listed slugs and units in each Bencher project before the first
publication. BMF contains metric values and measure identifiers, but no unit metadata. Bencher
can create measures automatically from BMF, but explicitly configuring their units avoids relying
on those defaults. The future publisher should verify these definitions before uploading.

The BMF file contains only metrics. Preserve the raw JSON artifact for source metadata,
parameters, environment, labels, and failures. Pass the tested revision and branch explicitly to
`bencher run`; checking out Benchy to build the converter must not cause results to be attributed
to Benchy's revision. Parameter changes that affect comparability require a different benchmark
identity or Bencher testbed.

## Planned publishing job

No new runner needs to be installed. Add a second job to
`.github/workflows/run-benchmarks.yml`, with this structure:

```yaml
jobs:
  benchmark:
    # Existing job on Alice, including raw result artifact upload.
    runs-on: ${{ fromJSON(inputs.runner-labels) }}
    # ...existing steps...

  publish:
    needs: benchmark
    runs-on: ubuntu-latest
    permissions:
      contents: read
    # ...download artifact, build converter, configure Bencher CLI, and submit...
```

`ubuntu-latest` requests a fresh GitHub-hosted machine. `needs: benchmark` waits until Alice's
job succeeds; failures still retain their raw artifact and summary, without entering the baseline.
The existing artifact name includes the run ID and attempt; download that exact artifact with
`actions/download-artifact` in the publisher. Build the converter from the trusted Benchy
checkout and treat the downloaded JSON as data. There is no reason to build or execute product
code in this job.

Define an optional `bencher-project` input and a named `BENCHER_API_KEY` secret in
`on.workflow_call`. Each product caller will supply:

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

This caller example is for the future publisher: the current reusable workflow does not yet
declare these settings. Only the publisher should reference the key in a step's environment.
An empty project input can leave publication disabled for repositories still using artifacts.

After downloading and converting the artifact, the publishing command will have this form:

```console
bencher run --project "$BENCHER_PROJECT" \
  --branch "$TESTED_BRANCH" --hash "$TESTED_COMMIT" \
  --testbed alice-bob-v1 --adapter json --file bencher.json
```

Set `BENCHER_API_KEY` in that step's environment from the forwarded secret. Set the branch and
commit from the benchmark's tested revision, and use a stable testbed for the Alice/Bob setup.
Keep the converted file as another diagnostic artifact so an upload can be retried without
rerunning the benchmark. Introduce GitHub checks/PR comments and regression thresholds after
verifying a manual GotaTun upload; those reporting features require additional job permissions.

## References

- [Bencher Metric Format](https://bencher.dev/docs/reference/bencher-metric-format/)
- [Project measures API](https://bencher.dev/docs/api/projects/measures/)
- [Bencher run](https://bencher.dev/docs/explanation/bencher-run/)
- [Sharing artifacts between jobs](https://docs.github.com/en/actions/tutorials/store-and-share-data)
- [Passing secrets to reusable workflows](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows)
