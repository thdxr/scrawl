#!/usr/bin/env bash
# Plays `scrawl lab stack` scenes and records each: a motion-lint trace of every frame and a
# screen recording of the whole main display (the lab covers it with a pretend screen).
#
#   perf/stack/record.sh <scrawl-binary> <out-dir> <variant> [--time-scale 0.25] [scene ...]
#
# Writes <out-dir>/<variant>-<scene>.mov and <out-dir>/runs/<variant>-<scene>/trace/*.jsonl.
# Nothing is captured, recorded or sent by Scrawl. Record on a Mac nobody is using (the lab
# covers its screen for the length of the scene).
set -euo pipefail

bin=$1
out=$2
variant=$3
shift 3
scale_args=()
if [[ ${1:-} == --time-scale ]]; then
  scale_args=(--time-scale "$2")
  shift 2
fi
scenes=("$@")
[[ ${#scenes[@]} -gt 0 ]] || scenes=(one three story)

for scene in "${scenes[@]}"; do
  name="$variant-$scene"
  caffeinate -u -t 60 >/dev/null 2>&1 &
  rm -f "$out/$name.mov"
  mkdir -p "$out/runs/$name/trace"
  log="$out/.$name.log"
  "$bin" lab stack "$scene" --variant "$variant" --trace "$out/runs/$name/trace/$name.jsonl" \
    ${scale_args[@]+"${scale_args[@]}"} >"$log" 2>&1 &
  lab=$!
  region=""
  for _ in {1..400}; do
    region=$(sed -n 's/^region //p' "$log" 2>/dev/null | head -1)
    [[ -n $region ]] && break
    sleep 0.05
  done
  [[ -n $region ]] || { echo "no region from the lab ($log)"; cat "$log"; kill "$lab"; exit 1; }
  sleep 0.3
  screencapture -v -x -R"$region" "$out/$name.mov" &
  rec=$!
  wait "$lab"
  sleep 0.3
  kill -INT "$rec" 2>/dev/null || true
  wait "$rec" 2>/dev/null || true
  echo "recorded $name"
done
