#!/usr/bin/env bash
# Records `scrawl lab stack` scenes on another Mac (default: pro) whose screen nobody is using.
#
#   perf/stack/pro-record.sh <scrawl-binary> <local-out-dir> <variant> <scene> [extra lab args…]
#
# `screencapture` over SSH has no Screen Recording grant, so the binary goes into a staging app
# signed (in the GUI session, where the keychain is) as dev.kitlangton.scrawl with the Developer
# ID: that matches the installed Scrawl.app's grant. The lab is opened as that app and records
# the display itself (`--record`). Holds /tmp/pro-gui.lock on the remote while it runs.
set -euo pipefail

bin=$1
out=$2
variant=$3
scene=$4
shift 4
host=${HOST:-pro}
app=/tmp/ScrawlStackLab.app
name="$variant-$scene"
mkdir -p "$out"

scp -q "$bin" "$host:/tmp/scrawl-stack-lab-bin"
ssh "$host" bash -s -- "$app" "$name" "$variant" "$scene" "$@" <<'REMOTE'
set -euo pipefail
app=$1; name=$2; variant=$3; scene=$4; shift 4
sum=$(shasum /tmp/scrawl-stack-lab-bin | cut -c1-12)
if [[ ! -f "$app/.sum" || $(cat "$app/.sum") != "$sum" ]]; then
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS"
  cp ~/Applications/Scrawl.app/Contents/Info.plist "$app/Contents/Info.plist"
  cp /tmp/scrawl-stack-lab-bin "$app/Contents/MacOS/scrawl"
  ~/bin/gui-exec codesign --force --sign "Developer ID Application: Christopher Langton (QC99C9JE59)" \
    --identifier dev.kitlangton.scrawl "$app"
  echo "$sum" > "$app/.sum"
fi
dir=/tmp/scrawl-stack-rec
mkdir -p "$dir"
rm -f "$dir/$name.mov" "$dir/$name.jsonl" "$dir/$name.log"
open -n -W -a "$app" --stdout "$dir/$name.log" --stderr "$dir/$name.log" --args \
  lab stack "$scene" --variant "$variant" --trace "$dir/$name.jsonl" --record "$dir/$name.mov" "$@"
ls -la "$dir/$name.mov" "$dir/$name.jsonl"
REMOTE
scp -q "$host:/tmp/scrawl-stack-rec/$name.mov" "$host:/tmp/scrawl-stack-rec/$name.jsonl" "$out/"
echo "recorded $out/$name.mov"
