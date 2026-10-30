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
  report_args+=(--github-actions "$BENCHY_GITHUB_TOKEN" --ci-only-thresholds)
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

# A product's trusted publishing workflow can select thresholds without changing the harness.
# Validate before making API calls. Do not read policy from untrusted result artifacts.
policy=${BENCHY_THRESHOLD_POLICY:-}
if [[ -n "$policy" ]]; then
  if ! jq -e '
    type == "object" and ((keys - ["minimum_samples","maximum_samples","measures"]) | length == 0) and
    (.minimum_samples | type == "number" and floor == . and . >= 1) and
    (.maximum_samples | type == "number" and floor == . and . <= 1000) and
    (.maximum_samples >= .minimum_samples) and
    (.measures | type == "object" and all(keys[]; test("^[a-z][a-z0-9-]{0,63}$")) and
      all(.[]; type == "object" and ((keys - ["lower","upper"]) | length == 0) and
        (has("lower") or has("upper")) and all(.[]; type == "number" and . >= 0)))
  ' <<< "$policy" > /dev/null; then
    echo 'Invalid BENCHY_THRESHOLD_POLICY' >&2
    exit 1
  fi
fi

threshold_args=()
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

  # Default percentage tests work from one prior sample; products can require more history.
  # Keep iperf's own CPU diagnostic: it changes with the traffic it generates.
  minimum_samples=1
  maximum_samples=30
  if [[ -n "$policy" ]]; then
    lower=$(jq -r --arg slug "$slug" '.measures[$slug].lower // "_"' <<< "$policy")
    upper=$(jq -r --arg slug "$slug" '.measures[$slug].upper // "_"' <<< "$policy")
    minimum_samples=$(jq -r '.minimum_samples' <<< "$policy")
    maximum_samples=$(jq -r '.maximum_samples' <<< "$policy")
  else
    lower=_
    upper=_
    case "$slug" in
      throughput|throughput-*)
        if [[ "$units" == 'Gb/s' ]]; then lower=0.05; fi
        ;;
      cpu-iperf|cpu-iperf-*) ;;
      cpu-*)
        if [[ "$units" == '% of one logical CPU' ]]; then upper=0.10; fi
        ;;
    esac
  fi
  if [[ "$lower" != _ || "$upper" != _ ]]; then
    threshold_args+=(--threshold-measure "$slug" --threshold-test percentage
      --threshold-min-sample-size "$minimum_samples" --threshold-max-sample-size "$maximum_samples"
      --threshold-lower-boundary "$lower" --threshold-upper-boundary "$upper")
  fi
done < <(jq -r 'keys[]' "$definitions_file")

# --github-actions makes the Bencher check fail on regression alerts. Keep successful
# publication successful so its report and workflow summary remain available on alerts.
bencher run --project "$BENCHER_PROJECT" \
  --branch "$tested_branch" --hash "$GITHUB_SHA" \
  --testbed "$BENCHER_TESTBED" --adapter json --file "$metrics_file" \
  --format json --quiet --attempts 3 "${report_args[@]}" "${threshold_args[@]}" > "$report_file"

report_id=$(jq -er '.uuid' "$report_file")
project_slug=$(jq -er '.project.slug' "$report_file")
report_url="https://bencher.dev/console/projects/$project_slug/reports/$report_id"
printf 'Bencher report: %s\n' "$report_url"
printf '\n## Bencher results\n\n[View report](%s)\n' "$report_url" >> "$GITHUB_STEP_SUMMARY"
