#!/usr/bin/env bash
# Publish already-converted metrics. Product code is never executed in this job.
set +x
set -euo pipefail

: "${BENCHER_API_KEY:?Pass the project API key as the BENCHER_API_KEY workflow secret}"
: "${BENCHER_PROJECT:?Set the BENCHER_PROJECT repository variable}"
: "${BENCHER_TESTBED:?Set a stable Bencher testbed}"
: "${GITHUB_SHA:?Missing tested commit}"
: "${GITHUB_REF_NAME:?Missing tested branch}"
: "${GITHUB_STEP_SUMMARY:?Missing workflow summary path}"

metrics_file=${1:?Expected metrics JSON path}
report_file=${2:?Expected report JSON path}

# BMF has no unit metadata. Create only measures used in this report, and refuse to
# silently relabel existing historical data if the configured units do not match.
while read -r slug units; do
  if ! jq -e --arg slug "$slug" 'any(.[]; has($slug))' "$metrics_file" > /dev/null; then
    continue
  fi
  measures=$(bencher measure list "$BENCHER_PROJECT" --search "$slug" --per-page 255 --attempts 3)
  if jq -e --arg slug "$slug" 'any(.[]; .slug == $slug)' <<< "$measures" > /dev/null; then
    if ! jq -e --arg slug "$slug" --arg units "$units" \
      'all(.[] | select(.slug == $slug); .units == $units)' <<< "$measures" > /dev/null; then
      echo "Bencher measure '$slug' has unexpected units; expected '$units'" >&2
      exit 1
    fi
  else
    bencher measure create "$BENCHER_PROJECT" --name "$slug" --slug "$slug" \
      --units "$units" --attempts 3 > /dev/null
  fi
done <<'MEASURES'
gigabits-per-second Gb/s
percent %
seconds s
count count
MEASURES

bencher run --project "$BENCHER_PROJECT" \
  --branch "$GITHUB_REF_NAME" --hash "$GITHUB_SHA" \
  --testbed "$BENCHER_TESTBED" --adapter json --file "$metrics_file" \
  --format json --quiet --attempts 3 > "$report_file"

report_id=$(jq -er '.uuid' "$report_file")
project_slug=$(jq -er '.project.slug' "$report_file")
report_url="https://bencher.dev/console/projects/$project_slug/reports/$report_id"
printf 'Bencher report: %s\n' "$report_url"
printf '\n## Bencher results\n\n[View report](%s)\n' "$report_url" >> "$GITHUB_STEP_SUMMARY"
