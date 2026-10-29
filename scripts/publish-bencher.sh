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

# workflow_run's GITHUB_SHA is the default branch, not the tested commit. Bencher
# uses this environment variable for the Check's SHA as well as the explicit --hash.
export GITHUB_SHA=${BENCHY_SOURCE_SHA:-$GITHUB_SHA}
tested_branch=${BENCHY_SOURCE_BRANCH:-$GITHUB_REF_NAME}
report_args=()
if [[ -n "${BENCHY_GITHUB_TOKEN:-}" ]]; then
  report_args+=(--github-actions "$BENCHY_GITHUB_TOKEN")
fi
if [[ -n "${BENCHY_PR_NUMBER:-}" ]]; then
  : "${BENCHY_GITHUB_TOKEN:?Missing GitHub reporting token}"
  : "${BENCHY_BASE_BRANCH:?Missing PR base branch}"
  : "${BENCHY_BASE_SHA:?Missing PR base commit}"
  report_args+=(--ci-number "$BENCHY_PR_NUMBER" --start-point "$BENCHY_BASE_BRANCH"
    --start-point-hash "$BENCHY_BASE_SHA" --start-point-reset)
fi

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

# Percentage-based regression thresholds, created for each run's branch and testbed.
# Throughput alerts on drops and CPU usage on increases; the observed run-to-run noise
# is roughly 3% for throughput and up to ~11% for the smallest CPU metric, so these
# boundaries leave a comfortable margin. History is sparse while the project is young,
# so the minimum sample size is 1; revisit both once a real history accumulates.
bencher run --project "$BENCHER_PROJECT" \
  --branch "$tested_branch" --hash "$GITHUB_SHA" \
  --testbed "$BENCHER_TESTBED" --adapter json --file "$metrics_file" \
  --threshold-measure gigabits-per-second --threshold-test percentage \
  --threshold-min-sample-size 1 --threshold-max-sample-size 64 \
  --threshold-lower-boundary 0.10 --threshold-upper-boundary _ \
  --threshold-measure percent --threshold-test percentage \
  --threshold-min-sample-size 1 --threshold-max-sample-size 64 \
  --threshold-lower-boundary _ --threshold-upper-boundary 0.25 \
  --thresholds-reset \
  --format json --quiet --attempts 3 "${report_args[@]}" > "$report_file"

report_id=$(jq -er '.uuid' "$report_file")
project_slug=$(jq -er '.project.slug' "$report_file")
report_url="https://bencher.dev/console/projects/$project_slug/reports/$report_id"
printf 'Bencher report: %s\n' "$report_url"
printf '\n## Bencher results\n\n[View report](%s)\n' "$report_url" >> "$GITHUB_STEP_SUMMARY"
