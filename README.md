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
- `benchy-lib` contains the temporary result artifact types shared by benchmark crates and the
  runner.
- `.github/workflows/run-benchmarks.yml` is the reusable workflow called by product repositories.

The per-run JSON files use the versioned result schema documented in
[`docs/result-schema-v1.md`](docs/result-schema-v1.md). They are uploaded as diagnostic artifacts
and printed verbatim in the workflow run summary for quick inspection. SQLite-backed storage,
migration tooling, and the egui results interface will consume the same schema.

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
