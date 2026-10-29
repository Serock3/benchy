#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$MOCK_LOG"
case "$1 $2" in
  'measure list')
    case "$MOCK_MODE" in
      api-failure) echo 'API unavailable' >&2; exit 1 ;;
      missing) echo '[]' ;;
      wrong-units) echo '[{"slug":"bits-per-second","units":"bits/s"},{"slug":"percent","units":"cores"}]' ;;
      *) echo '[{"slug":"bits-per-second","units":"bits/s"},{"slug":"percent","units":"%"}]' ;;
    esac
    ;;
  'measure create') echo '{}' ;;
  'run --project')
    if [[ "$MOCK_MODE" == upload-failure ]]; then
      echo 'Upload failed' >&2
      exit 1
    fi
    echo '{"uuid":"report-id","project":{"slug":"gotatun-test"}}'
    ;;
  *) echo 'Unexpected Bencher command' >&2; exit 1 ;;
esac
