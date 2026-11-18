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
docker build -f "$context/scripts/stability/Dockerfile" --iidfile "$context/image-id" "$context"
image_id=$(cat "$context/image-id")
mkdir -p "$root/docs/evidence/stability"
mkdir "$root/docs/evidence/stability/$sha-$mode"
if [ "$mode" = smoke ]; then
  set -- node scripts/stability/smoke.mjs
else
  set -- node scripts/stability/run.mjs
fi
docker run --rm --cpus=2 --memory=4g --shm-size=1g --network=none -e STABILITY_SHA="$sha" -e STABILITY_CONTAINER_IMAGE="$image_id" -v "$root/docs/evidence/stability/$sha-$mode:/evidence" "$image_id" "$@"
