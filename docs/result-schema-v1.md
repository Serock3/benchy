# Benchmark result schema v1

Each benchmark invocation writes one UTF-8 JSON document. Version 1 separates stable machine
identifiers from presentation text so labels can improve without splitting a historical series.

```json
{
  "schema_version": 1,
  "repository": "gotatun",
  "name": "gotatun-throughput",
  "commit": "abc123",
  "branch": "main",
  "commit_message": "Measure throughput",
  "description": "GotaTun tunnel throughput measured with iperf3",
  "date": "2026-09-25T12:00:00Z",
  "measurements": {
    "cpu.gotatun.up": {
      "label": "UP GotaTun CPU",
      "unit": "percent",
      "value": 282.1
    },
    "throughput.receiver": {
      "label": "Receiver throughput",
      "unit": "bits_per_second",
      "value": 2730000000.0
    }
  },
  "run_id": "123456",
  "status": "success",
  "parameters": {
    "duration_seconds": "30",
    "mtu": "1440"
  },
  "environment": {
    "RUNNER_NAME": "benchy-alice",
    "RUNNER_OS": "Linux"
  }
}
```

## Identity and metadata

The identity of a time series is the tuple `repository`, `name`, and measurement ID. These values
must remain stable after results have been published. `description` and measurement `label` are
presentation text and may change without creating a new series.

`commit` identifies the source revision under test. `branch` is the selected source ref and `date`
is the UTC time at which the benchmark started. `run_id` is optional and identifies the CI workflow
run when one exists.

`status` is either `success` or `failed`. A failed result may include an `error` string and may have
no measurements. Results from successful and failed runs retain their parameters and environment
metadata.

## Measurements

Measurement IDs are lowercase dot-separated identifiers. Each segment starts with an ASCII letter
and may then contain lowercase ASCII letters, digits, `_`, or `-`. For example:

- `throughput.sender`
- `cpu.gotatun.up`
- `latency.p95_seconds`

The supported v1 units are:

- `bits_per_second`
- `percent`, normalized so 100% is one logical CPU core
- `seconds`
- `count`

Measurement values are finite JSON numbers. Human-facing code uses `label`; storage, comparisons,
and chart selection use the measurement ID.

## Parameters and environment

`parameters` contains benchmark-controlled inputs that can affect comparability. `environment`
contains an allowlisted set of execution metadata. Both maps use string values in v1. Secrets must
never be recorded in either map.

## Compatibility

`schema_version` is required. Readers must reject unsupported versions instead of attempting a
best-effort parse. The unversioned JSON produced during the initial vertical slice and the older
app-bench JSONL format are pre-v1 inputs; migration tooling may convert them explicitly, but they
are not valid v1 documents.
