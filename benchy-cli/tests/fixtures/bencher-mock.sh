#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$MOCK_LOG"
case "$1 $2" in
  'measure list')
    case "$MOCK_MODE" in
      api-failure) echo 'API unavailable' >&2; exit 1 ;;
      missing) echo '[]' ;;
      wrong-units) echo '[{"slug":"cpu-gotatun-up","units":"cores"}]' ;;
      wrong-throughput-units) echo '[{"slug":"throughput-receiver","units":"bits/s"},{"slug":"cpu-gotatun-up","name":"UP GotaTun CPU (logical CPU %)","units":"% of one logical CPU"}]' ;;
      legacy-units) echo '[{"slug":"bits-per-second","units":"bits/s"},{"slug":"percent","units":"%"}]' ;;
      *) echo '[{"slug":"throughput-receiver","name":"Receiver throughput (Gb/s)","units":"Gb/s"},{"slug":"cpu-gotatun-up","name":"UP GotaTun CPU (logical CPU %)","units":"% of one logical CPU"}]' ;;
    esac
    ;;
  'measure create') echo '{}' ;;
  'run --project')
    if [[ -n "${MOCK_EXPECT_SHA:-}" && "$GITHUB_SHA" != "$MOCK_EXPECT_SHA" ]]; then
      echo 'Check would target the wrong commit' >&2
      exit 1
    fi
    if [[ "$MOCK_MODE" == upload-failure ]]; then
      echo 'Upload failed' >&2
      exit 1
    fi
    echo '{"uuid":"report-id","project":{"slug":"gotatun-test"}}'
    ;;
  *) echo 'Unexpected Bencher command' >&2; exit 1 ;;
esac
