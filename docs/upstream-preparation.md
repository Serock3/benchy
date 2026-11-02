# Upstream review preparation

Preparation is confined to local branches and the test repositories. No upstream
branches, PRs, ownership or settings have been changed. Existing fork `main`
branches remain untouched. These are review candidates, not an approved rollout.

## Architecture and review boundaries

Product repositories own separate Cargo workspaces and workload definitions.
Benchy discovers and builds one selected benchmark package at a time, manages the
shared runtime helpers, and records versioned JSON artifacts. Alice controls Bob
over SSH and is the only host sampled for CPU/profiling. Traffic direction is
relative to Alice. A single organization runner queues both repositories; a file
lock also excludes manual runs. Publishing runs separately on GitHub-hosted
infrastructure with the Bencher project key. Profiling remains separate from the
uninstrumented throughput measurements.

The preparation branches are based on the upstream revisions fetched read-only on
2026-11-02. Source, manifests, workflows, tests and documentation below are counted;
generated lockfiles are excluded. Counts are approximate review scope, not just
new executable code. The framework total includes existing code needing review.

| Slice | Test repository / branch | Review scope |
| --- | --- | --- |
| Framework | `Serock3/benchy`, `prep/framework` | ~5,100 lines, including ~4,200 Rust and tests |
| Core GotaTun | `serock3-bench/gotatun`, `review/gotatun-benchmarks` | ~680 lines |
| Hotpath | same, `review/gotatun-hotpath`, above core | ~280 lines; 13 instrumentation lines in product Rust |
| Dial9 | same, `review/gotatun-dial9`, above hotpath | ~1,460 lines |
| App workloads | `serock3-bench/mullvadvpn-app`, `review/app-benchmarks` | ~1,330 lines |

App support is shared between both UDP sizes; each size/direction wrapper is tiny.
Review the common controller once, then the fragmentation parameters, then the
additional kernel WireGuard lifecycle (~430 lines). Dial9 depends on the shared
framework but does not enable hotpath instrumentation; its placement above hotpath
is a proposed landing order, not a runtime requirement.

## Preparation changes

- Shared owned local/SSH processes: recorded PIDs, readiness on either log stream,
  drained output, exact listener ownership, bounded shutdown and surfaced errors.
  No broad process-name or existing-port-owner killing.
- Shared Linux process CPU sampling and throughput/iperf CPU/direction definitions.
  CPU is a percentage of one logical CPU, across all process threads.
- Hotpath report conversion and product instrumentation are opt-in; numeric
  endpoints do not depend on hotpath when built individually.
- GotaTun numeric, hotpath and Dial9 use AWS-LC, matching the product default and
  root lockfile's AWS-LC 1.16.2. MASQUE already uses AWS-LC in its root workspace.
- Six app workloads split three scenarios into upload/download, collecting only
  Alice's local iperf/proxy CPU and throughput. Download UDP also records local
  receiver jitter/loss. WireGuard CPU refers to the userspace MASQUE process,
  excluding kernel WireGuard.
- Removed duplicate Dial9 process/lock helpers. The obsolete ping workload and
  Criterion experiment are absent; ping remains only as tunnel readiness.
- Configuration/measurement failures write failed result artifacts. Cleanup errors
  cannot turn a failed or unclean run into success.

## Measurement interpretation and operational limits

UDP sender rate is offered load, not delivered goodput. Upload can hide downstream
loss. At a requested 10 Gb/s, the validation downloads showed about 11% packet loss
for 1100-byte datagrams and 66% for 2200-byte datagrams. These are stress workloads;
do not interpret an upload rate as delivered throughput or that loss as a newly
introduced correctness regression. MASQUE will remain a coupled end-to-end test;
isolating Alice's upload/download processing capacity from Bob is not a goal for
this protocol. Interpret the local throughput, CPU and available receiver loss
together. The upload sender rate alone cannot support delivery-regression alerts.
The current code preserves Alice-only observations and documents the limit.

Throughput uncertainty comes from local one-second intervals, assuming negligible
between-run variance. CPU has no uncertainty bounds. Fresh AWS-LC/app-directional
baselines are needed before calibrating regression thresholds. Prepared trusted
publishers select `alice-bob-aws-lc-v1` and `alice-bob-app-v2`; these settings only
take effect when those publishers reach the fork's default branch. Validation
branch runs currently use the existing default-branch publisher and its testbed,
in separate Bencher branch series. Renaming the workflow on a branch does not
suppress its default-branch `workflow_run` publisher.

A controller hard kill, lost SSH session, or interruption during startup can still
leave remote resources. Numeric/Dial9 endpoints have leases; generic proxy programs
do not. Normal errors and Ctrl-C during workloads attempt owned-resource cleanup.
Include lost-controller recovery in operational review; no automatic global cleanup
is introduced.

## Optional GotaTun I/O follow-up

Keep the existing TCP iperf upload/download benchmarks for the initial merge. A
separate experiment can reduce the download workload's dependence on Bob's
encryption speed without replacing the real network path:

1. Establish a real WireGuard session and prepare fresh encrypted packets on Bob
   before the measured phase.
2. Send those packets over the physical link using efficient batched sends, with
   a sustained traffic pattern rather than a single queue-filling burst.
3. Keep Alice's normal GotaTun device, kernel, NIC and TUN path. Measure local CPU
   and payload delivered to a local receiver, including sequence-based loss counts.
4. Verify that Bob's offered traffic exceeds Alice's processing capacity. Bob's
   socket/NIC transmission capacity and the link remain possible bottlenecks.

A small custom UDP workload may replace TCP iperf for this experiment; the
WireGuard wire format stays unchanged. Encrypted buffers cannot be replayed in a
loop: each packet needs a unique counter in a valid session, and buffer memory
must be budgeted. Production replay protection and session handling stay enabled.
Traffic must cross the actual NICs; localhost or an in-memory transport would omit
the kernel/NIC I/O behavior this experiment is intended to measure.

This follow-up does not block reviewing or merging the current benchmarks.

## History cleanup proposal

Local `archive/preparation-start-20261102` refs preserve all three original main
snapshots. Product review branches contain clean feature commits on current upstream,
without the GSO PR, intentional regression demo, merge experiments or prototype
history. They can become the proposed upstream review slices after human scrutiny.
No PRs have been opened for them.

Before replacing any fork main, publish backup refs in the test fork, inspect the
full tree diff, account for open test PRs, and explicitly choose the new default
branch tip. Replacing main would require a coordinated force-with-lease push and
realignment of those test PRs; it has not been done. Preserve the experiment branches
separately rather than mixing GSO/demo changes into benchmark preparation.

For Benchy, a fresh root snapshot can replace the discarded SQLite/GUI development
history after review. Retain an archive ref containing the old workflow SHAs, then
repin caller workflow/checkout references to the chosen framework commit. Ownership
transfer and repository reference changes are later deployment tasks, outside this
preparation.

## Validation

- Framework: 65 tests passed with all features on Rust 1.95; workspace Clippy passed.
- GotaTun: core benchmark tests, 47 product library tests, workspace Clippy, and
  opt-in hotpath workload/call-count/report checks passed. The individually built
  upload dependency graph contains AWS-LC and no Ring or hotpath.
- App: workspace tests/Clippy/discovery passed for all six directional definitions.
- Dial9: four local tests, Clippy and verification-script syntax check passed.
- Invalid-duration checks for both product workspaces returned failure and wrote
  failed JSON artifacts before touching the machines.
- [GotaTun numeric validation](https://github.com/serock3-bench/gotatun/actions/runs/41471212836)
  passed both 30-second directions (~2.7 Gb/s).
- [App validation](https://github.com/serock3-bench/mullvadvpn-app/actions/runs/41471222546)
  passed all six workloads; raw reports include the required local receiver stats.
- Both numeric runs passed the existing hosted publication workflows as isolated
  `validation/preparation` Bencher branch series. No main baseline was replaced.
- [Hotpath validation](https://github.com/serock3-bench/gotatun/actions/runs/41472924821)
  and its hosted publisher passed.
- [Dial9 validation](https://github.com/serock3-bench/gotatun/actions/runs/41472037789)
  passed with three seconds per direction. CLI 0.5.6 decoded both trace segments,
  27 converted tracing events, orderly shutdown, ~3-second phases with on-CPU
  samples and symbolized GotaTun stacks in each direction; CPU backend was `perf`.
  Default workflow duration remains five seconds per direction.
- After the successful runs neither host retained benchmark processes or tunnel
  interfaces; Alice had about 7 GB free. The legacy runner remains stopped.
- Live occupied-port validation produced a failed result, preserved the unrelated
  listener, and removed both TUN interfaces. Ctrl-C during a real transfer produced
  a failed result and removed both endpoints, interfaces and the owned iperf server.

The latest app follow-up requires receiver jitter/loss rather than silently
omitting them. The successful live artifact already contains both fields for
both UDP download sizes; the follow-up also passed local Clippy. The first app
preparation subject was reworded to satisfy the app's 72-character commit-subject
policy, preserving its tree and an archive ref of the previous tip.

Pending rollout work includes maintainer review, fresh baseline/threshold
calibration, organization/account ownership and
repository reference updates. No upstream rollout has been attempted.
