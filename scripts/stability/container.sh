#!/bin/sh
set -eu
mode=${1:-proof}
case "$mode" in proof|smoke) ;; *) echo 'Expected proof or smoke' >&2; exit 2;; esac
root=$(git rev-parse --show-toplevel)
sha=$(git rev-parse HEAD)
context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
# Archive the fixed commit: no host dependencies, untracked samples or dirty edits.
git archive "$sha" | tar -x -C "$context"
harness=$(cd "$(dirname "$0")" && pwd)
dockerfile="$context/scripts/stability/Dockerfile"
if [ "$#" -eq 2 ] && [ "$2" = hosted ]; then
  # The selected workflow revision owns the harness. The candidate owns the app and tests.
  dockerfile="$harness/Dockerfile"
fi
docker build -f "$dockerfile" --iidfile "$context/image-id" "$context"
image_id=$(cat "$context/image-id")
mkdir -p "$root/docs/evidence/stability"
mkdir "$root/docs/evidence/stability/$sha-$mode"
set -- --rm --cpus=2 --memory=4g --shm-size=1g --network=none \
  -e STABILITY_SHA="$sha" -e STABILITY_CONTAINER_IMAGE="$image_id" \
  -v "$root/docs/evidence/stability/$sha-$mode:/evidence"
if [ "$mode" = proof ]; then
  set -- "$@" -e STABILITY_PROJECT -e STABILITY_REPEATS -e STABILITY_SPECS -e STABILITY_GREP -e STABILITY_REPEAT_EACH
fi
if [ "$dockerfile" = "$harness/Dockerfile" ]; then
  set -- "$@" -v "$harness/run.mjs:/harness/run.mjs:ro" \
    -e STABILITY_RUNNER -e STABILITY_HARNESS_SHA="$(git -C "$harness" rev-parse HEAD)"
  script=/harness/run.mjs
else
  script=scripts/stability/run.mjs
fi
if [ "$mode" = smoke ]; then
  script=scripts/stability/smoke.mjs
fi
docker run "$@" "$image_id" node "$script"
