# Benchy

Benchy is the shared framework for Mullvad's system benchmarks. Benchmark definitions stay next to
the code under test, while this repository provides benchmark discovery, execution, result
recording, and the reusable GitHub Actions workflow.

The first vertical slice is the two-host GotaTun throughput benchmark. Alice runs the controller on
a self-hosted GitHub Actions runner, deploys the benchmark endpoint to Bob over their local link,
establishes the tunnel, and measures it with iperf3.

## Components

- `benchy-cli` discovers packages with `[package.metadata.benchy]` and runs the requested benchmarks
  sequentially.
- `benchy-runner` provides result recording, GitHub metadata, iperf parsing, and a cross-workflow
  machine lock.
- `benchy-lib` contains the versioned result artifact types shared by benchmark crates and the
  runner.
- `.github/workflows/run-benchmarks.yml` is the reusable workflow called by product repositories.
- `.github/workflows/publish-benchmarks.yml` publishes completed runs from a separate trusted
  `workflow_run` workflow, including GitHub checks and PR comments.

The per-run JSON files use the versioned result schema documented in
[`docs/result-schema-v1.md`](docs/result-schema-v1.md). They are uploaded as diagnostic artifacts
and printed verbatim in the workflow run summary for quick inspection. Alice runs benchmarks and
writes result files; there is no persistent results database or web service to deploy there.

Bencher Cloud provides metric history and plots. `benchy-cli export-bencher` converts result files
to Bencher Metric Format (BMF). When a Bencher project is configured, a GitHub-hosted publishing
workflow submits successful results, creates a GitHub check, and comments on PRs. Both raw JSON
and Bencher diagnostics remain available as artifacts. Percentage-based regression thresholds
alert on throughput drops or CPU-usage increases without failing the publishing workflow.

## Local layout

Product benchmark workspaces use path dependencies during development, so the product and framework
repositories are sibling checkouts:

```text
workspace/
├── benchy/
└── gotatun/
```

List or run the GotaTun benchmarks from the workspace directory:

```console
cargo run --manifest-path benchy/Cargo.toml --package benchy-cli -- list \
  --manifest-path gotatun/benchmarks/Cargo.toml

cargo run --manifest-path benchy/Cargo.toml --package benchy-cli -- run \
  --manifest-path gotatun/benchmarks/Cargo.toml \
  --benchmarks default
```

## Bencher Cloud setup and rollout

Create a public Bencher project for each product repository. In **each product repository** on
GitHub, open **Settings → Secrets and variables → Actions** and configure:

| Kind | Name | Value |
| --- | --- | --- |
| Repository secret | `BENCHER_API_KEY` | The project API key's value, without `export BENCHER_API_KEY=` or surrounding quotes |
| Repository variable | `BENCHER_PROJECT` | The Bencher project slug, e.g. `gotatun-test` |

The GotaTun and mullvadvpn-app repositories use the same configuration names with different
values: each key is scoped to its own Bencher project. Store the credentials in the repository
that launches the workflow (currently the product forks), not in the shared Benchy repository or
on Alice. The `export BENCHER_API_KEY=...` command shown by Bencher is for local CLI use.

The product's separate publisher passes the project slug as `bencher-project` and explicitly
forwards the named secret; see the example in [`docs/bencher.md`](docs/bencher.md). An empty project
slug disables publication. The optional `bencher-testbed` input defaults to `alice-bob-v1`.
The publisher downloads the benchmark artifact, converts it, verifies the units, and submits it
using the project key. Only the publishing step receives the key. The benchmark workflow runs on
Alice with a read-only repository token and no Bencher secrets. The trusted publisher runs on
GitHub with permission to read artifacts and write checks/PR comments.

GotaTun runs benchmarks on pushes to `main`, PRs targeting `main`, and manual dispatch. Configure
**Settings → Actions → General → Require approval for all external contributors** before enabling
the PR trigger. Internal PRs run automatically; external PRs wait for a maintainer to approve
workflows from the PR page. Approval authorizes executing that revision on Alice/Bob; it does not
sandbox the code or change host-level SSH/sudo access.

Roll out with a manual GotaTun run, verify units and source metadata against the raw JSON, then
verify a PR check/comment and the external approval gate. Repeat for mullvadvpn-app after the
pilot. Legacy app-bench
JSON can be evaluated for a one-time import; results from the experimental SQLite store do not
need migration. Keep raw artifacts for debugging and retrying publication; GitHub artifact
retention is finite, so they are not a permanent archive.

See Bencher's [custom benchmark guide](https://bencher.dev/docs/how-to/track-custom-benchmarks/)
and [GitHub Actions guide](https://bencher.dev/docs/how-to/github-actions/). The local conversion
command, measure mapping, and publishing job are documented in
[`docs/bencher.md`](docs/bencher.md).

## Future profiling

Consider [Dial9](https://github.com/dial9-rs/dial9) and
[hotpath-rs](https://github.com/pawurb/hotpath-rs) as possible profiling integrations. Profiling
should be opt-in initially, since instrumentation may affect the benchmark measurements.
Associate profile artifacts with the benchmark ID, commit, workflow run, parameters, and endpoint
role so developers can find them alongside numeric results. Start with Actions artifacts and
links from the workflow summary; evaluate longer-term storage and viewers when profiling is
implemented. No profiler is currently integrated into Benchy.

## Controller prerequisites

Alice needs Rust, `iproute2`, `iperf3`, OpenSSH, and passwordless access to the privileged endpoint
operations. Bob needs the runtime tools and an SSH account reachable from Alice. The initial
transport is SSH; it can later be replaced by a dedicated peer agent without changing benchmark
discovery or workflow triggers.

Only one Actions runner should have the `benchy` label. `benchy-runner` also acquires
`/tmp/benchy.lock`, preventing two new-framework runs from using Alice and Bob simultaneously. The
legacy shell runner does not acquire this lock and must be disabled during smoke tests.

Runtime configuration is supplied through `BENCHY_PEER`, `BENCHY_ALICE_ADDRESS`, and
`BENCHY_BOB_ADDRESS` repository variables. WireGuard key material is generated for each run.

During initial development the product workflow follows `Serock3/benchy@main`. Pin it to an
immutable commit before using the workflow from the production repositories.
