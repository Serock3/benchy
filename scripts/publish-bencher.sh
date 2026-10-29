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
definitions_file=${3:?Expected measure definitions JSON path}

# BMF has no display names or units. The trusted converter supplies definitions,
# keyed by stable metric IDs, separately from values. Validate the complete set first.
jq -e --slurpfile definitions "$definitions_file" \
  '([.[] | keys[]] | unique) == ($definitions[0] | keys)' "$metrics_file" > /dev/null
jq -e 'all(.[]; (.name | type == "string" and length > 0) and
  (.units | type == "string" and length > 0))' "$definitions_file" > /dev/null

while IFS= read -r slug; do
  name=$(jq -er --arg slug "$slug" '.[$slug].name' "$definitions_file")
  units=$(jq -er --arg slug "$slug" '.[$slug].units' "$definitions_file")
  measures=$(bencher measure list "$BENCHER_PROJECT" --search "$slug" --per-page 255 --attempts 3)
  if jq -e --arg slug "$slug" 'any(.[]; .slug == $slug)' <<< "$measures" > /dev/null; then
    if ! jq -e --arg slug "$slug" --arg units "$units" \
      'all(.[] | select(.slug == $slug); .units == $units)' <<< "$measures" > /dev/null; then
      echo "Bencher measure '$slug' has unexpected units; expected '$units'" >&2
      exit 1
    fi
    # Project keys cannot rename existing measures. A display-label change can be
    # applied in Bencher with a user key; the stable slug preserves the series.
    if ! jq -e --arg slug "$slug" --arg name "$name" \
      'all(.[] | select(.slug == $slug); .name == $name)' <<< "$measures" > /dev/null; then
      printf "Bencher measure '%s' has a different display name; expected '%s'\n" "$slug" "$name" >&2
    fi
  else
    bencher measure create "$BENCHER_PROJECT" --name "$name" --slug "$slug" \
      --units "$units" --attempts 3 > /dev/null
  fi
done < <(jq -r 'keys[]' "$definitions_file")

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
