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
- `benchy-store` validates and transactionally ingests result documents into SQLite.
- `benchy-serve` exposes stored results as JSON and serves the web frontend.
- `benchy-gui` is the egui frontend, with separate plots for each measurement unit.
- `.github/workflows/run-benchmarks.yml` is the reusable workflow called by product repositories.

The per-run JSON files use the versioned result schema documented in
[`docs/result-schema-v1.md`](docs/result-schema-v1.md). They are uploaded as diagnostic artifacts
and printed verbatim in the workflow run summary for quick inspection. The reusable workflow also
ingests them into a persistent SQLite database for the results interface.

By default, the database is stored at
`$XDG_DATA_HOME/benchy/results.sqlite3`, or `$HOME/.local/share/benchy/results.sqlite3` when
`XDG_DATA_HOME` is unset. A caller can override it with the reusable workflow's `database-path`
input. The database uses WAL mode, foreign-key enforcement, and transactional batch ingestion.
Re-running the same GitHub workflow attempt updates its existing rows instead of duplicating them.

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

cargo run --manifest-path benchy/Cargo.toml --package benchy-cli -- ingest \
  --database /tmp/benchy-results.sqlite3 \
  --result-dir benchmark-results

cargo run --manifest-path benchy/Cargo.toml --package benchy-cli -- inspect \
  --database /tmp/benchy-results.sqlite3
```

`inspect` prints the newest runs, their status, commit, and measurement count. Add `--json` for
machine-readable output.

## Results interface

Build the WASM frontend and start the server from the Benchy repository root:

```console
rustup target add wasm32-unknown-unknown
cargo install --locked trunk
cd benchy-gui
trunk build --release
cd ..
cargo run --release --package benchy-serve
```

The server reads the same default database as the workflow and serves Rocket's default address,
`http://127.0.0.1:8000`. Configure it with:

- `BENCHY_DATABASE_PATH` for a different SQLite database.
- `BENCHY_FRONTEND_PATH` for a different frontend distribution directory.
- Rocket configuration such as `ROCKET_ADDRESS` and `ROCKET_PORT` for the listening socket.

The API endpoints are `/api/benchmarks` and `/api/health`. Database access is read-only and runs on
blocking worker threads so concurrent WAL writes from benchmark workflows remain safe.

For Alice, build the frontend on a development machine and copy `benchy-gui/dist` to a persistent
Benchy checkout at `/home/mole/benchy/benchy-gui/dist`. Build the server in that checkout with
`cargo build --locked --release --package benchy-serve`. The example
[`deploy/benchy-serve.service`](deploy/benchy-serve.service) runs it as `mole` on
`127.0.0.1:8001`, leaving the existing web service alone. Install it with
`sudo cp deploy/benchy-serve.service /etc/systemd/system/`, then
`sudo systemctl daemon-reload` and `sudo systemctl enable --now benchy-serve.service`.
From a workstation that can SSH to Alice, `ssh -L 8001:127.0.0.1:8001 mole@benchy-alice` makes
the interface available at `http://127.0.0.1:8001`.

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
