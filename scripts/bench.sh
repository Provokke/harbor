#!/usr/bin/env bash
# Benchmarks: startup time, op overhead, file read throughput (release build).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -q
BIN=target/release/harbor
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
echo 'console.log("hi")' > "$TMP/hello.js"
echo 'const x: number = 1; console.log(x)' > "$TMP/hello.ts"
measure() { # name, runs, cmd...
  local name=$1 runs=$2; shift 2
  "$@" >/dev/null # warm
  local start end
  start=$(date +%s%N)
  for _ in $(seq "$runs"); do "$@" >/dev/null; done
  end=$(date +%s%N)
  printf '%-34s %10.2f ms (mean of %d)\n' "$name" "$(echo "($end-$start)/$runs/1000000" | bc -l)" "$runs"
}
echo "== startup =="
measure "harbor eval '1'" 50 $BIN eval 1
measure "harbor run hello.js" 50 $BIN run "$TMP/hello.js"
measure "harbor run hello.ts (oxc strip)" 50 $BIN run "$TMP/hello.ts"
echo "== ops & I/O =="
$BIN run --allow-read="$TMP" --allow-write="$TMP" benches/ops.js "$TMP"
