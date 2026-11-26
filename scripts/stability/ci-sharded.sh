#!/usr/bin/env bash
set -euo pipefail

accounting_script=$1
evidence=$2
browser=$3
shard=$4

[[ "$(git rev-parse HEAD)" == "$CANDIDATE" ]]
mkdir "$evidence"
for ((repetition = 1; repetition <= STABILITY_REPEATS; repetition++)); do
  echo "::group::$browser shard $shard/2, repetition $repetition/$STABILITY_REPEATS"
  receipt="$evidence/$repetition"
  mkdir "$receipt"
  tmp=$(mktemp -d)
  args=(--project="$browser" --shard="$shard/2" --workers=2)
  # A filtered control still has to cover the full, unfiltered inventory.
  if [[ -n "$STABILITY_GREP" ]]; then
    args+=(--grep="$STABILITY_GREP")
  fi
  status=0
  # The inner shell receives the inventory path and Playwright arguments positionally.
  # shellcheck disable=SC2016
  TMPDIR="$tmp" GROOVIQ_E2E_JSON="$receipt/report.json" timeout --kill-after=15s 9m bash -e -c '
    inventory=$1
    shift
    npm run --silent e2e -- --list --project=webkit --project=chromium --reporter=json > "$inventory"
    npm run e2e -- "$@"
  ' bash "$receipt/inventory.json" "${args[@]}" || status=$?
  outcome=success
  if ((status != 0)); then
    outcome=failure
  fi
  # Actions sets GITHUB_SHA to the dispatch ref. The receipt must identify the tested checkout.
  GITHUB_SHA="$CANDIDATE" SUITE_OUTCOME="$outcome" node "$accounting_script" receipt "$receipt" "$browser" "$shard" 2
  rm -f -- "$receipt/inventory.json" "$receipt/report.json"
  rm -rf -- "$tmp"
  echo "::endgroup::"
  if ((status != 0)); then
    exit "$status"
  fi
  # Successful repetitions need only their receipts. A failed one leaves its traces and journal.
  rm -rf -- e2e-results playwright-report
done
