#!/usr/bin/env bash
#
# Benchmark indexing a WACZ: wall time, peak RSS, per-phase timing, and the
# resulting index footprint. Optionally compares a baseline git ref against the
# current checkout (the before/after for the scale-tuning work).
#
# Usage:
#   scripts/bench-ingest.sh <file.wacz> [baseline-git-ref]
#
# Examples:
#   scripts/bench-ingest.sh archive-old/attar.wacz            # current build only
#   scripts/bench-ingest.sh archive-old/attar.wacz main~8     # baseline vs current
#
# Phases come from the indexer's own debug timers (RUST_LOG):
#   read+extract  fetch each record + HTML/PDF -> text   (read_ms)
#   index         tokenize + add to the writer buffer    (build_ms)
#   checksum      fixity hash of the WACZ                 (sha_ms)
#   commit        Tantivy segment flush to disk          (commit_ms)
#
# Peak RSS + wall time come from /usr/bin/time (this script assumes macOS's
# `-l`; on GNU/Linux swap to `/usr/bin/time -v` and adjust the field names).

set -euo pipefail

WACZ="${1:?usage: bench-ingest.sh <file.wacz> [baseline-git-ref]}"
BASELINE_REF="${2:-}"
[ -f "$WACZ" ] || { echo "no such WACZ: $WACZ" >&2; exit 1; }
WACZ="$(cd "$(dirname "$WACZ")" && pwd)/$(basename "$WACZ")" # absolutize

REPO="$(git rev-parse --show-toplevel)"
cd "$REPO"

# Strip ANSI colour codes (tracing emits them even when redirected).
strip_ansi() { sed $'s/\x1b\\[[0-9;]*m//g'; }

# Sum a debug field (e.g. read_ms) across all WACZ log lines, in milliseconds.
# Tolerant of no matches (grep exit 1 would otherwise trip `set -e`).
sum_ms() {
  strip_ansi <"$2" | { grep -oE "$1=[0-9]+" || true; } | awk -F= '{s+=$2} END {printf "%d", s+0}'
}
sec() { awk "BEGIN {printf \"%.2f\", $1/1000}"; } # ms -> s

# Run one binary against a fresh temp home; print a labeled report.
bench_one() {
  local bin="$1" label="$2"
  local home log; home="$(mktemp -d)"; log="$(mktemp)"

  # Fresh temp home per run, so no --force is needed (nothing is pre-indexed) —
  # and --force / `stats` don't exist on pre-tuning baselines anyway.
  RUST_LOG="indice_lib::index=debug" /usr/bin/time -l \
    "$bin" index "$WACZ" --home "$home" --collection bench \
    >/dev/null 2>"$log" || { echo "index failed ($label):" >&2; tail -20 "$log" >&2; exit 1; }

  local wall rss read_ms index_ms sha_ms commit_ms
  wall="$(awk '/ real/ {for (i=1;i<=NF;i++) if ($i=="real") print $(i-1)}' "$log" | tail -1)"
  rss="$(awk '/maximum resident set size/ {print $1}' "$log" | tail -1)"
  read_ms="$(sum_ms read_ms "$log")"
  index_ms="$(sum_ms build_ms "$log")"
  sha_ms="$(sum_ms sha_ms "$log")"
  commit_ms="$(sum_ms commit_ms "$log")"

  echo "── $label ─────────────────────────────────────────"
  printf "  wall:      %ss\n" "$wall"
  printf "  peak RSS:  %.1f MB\n" "$(awk "BEGIN {print ${rss:-0}/1048576}")"
  echo "  phases:"
  printf "    read+extract  %6ss\n" "$(sec "${read_ms:-0}")"
  printf "    index         %6ss\n" "$(sec "${index_ms:-0}")"
  printf "    checksum      %6ss\n" "$(sec "${sha_ms:-0}")"
  printf "    commit        %6ss\n" "$(sec "${commit_ms:-0}")"
  echo "  footprint:"
  printf "    index size:  %s\n" "$(du -sh "$home/index/full_text" 2>/dev/null | cut -f1)"
  # `indice stats` (per-file-type breakdown) exists only on tuned builds; skip it
  # gracefully on a pre-tuning baseline (the du total above is the comparable one).
  local statsout
  if statsout="$("$bin" stats --home "$home" 2>/dev/null)"; then
    echo "$statsout" | sed 's/^/    /'
  fi
  echo

  rm -rf "$home" "$log"
}

echo "Benchmark: indexing $(basename "$WACZ") ($(du -h "$WACZ" | cut -f1))"
echo

echo "Building current (release)…" >&2
cargo build --release -q
CUR="$REPO/target/release/indice"

if [ -n "$BASELINE_REF" ]; then
  WT="$(mktemp -d)"
  # Clean up the worktree even if the build or benchmark below fails.
  trap 'git worktree remove --force "$WT" 2>/dev/null || true; rm -rf "$WT"' EXIT
  echo "Building baseline $BASELINE_REF in a worktree…" >&2
  git worktree add -q --detach "$WT" "$BASELINE_REF"
  ( cd "$WT" && cargo build --release -q )
  bench_one "$WT/target/release/indice" "baseline ($BASELINE_REF)"
fi

bench_one "$CUR" "current ($(git rev-parse --short HEAD))"
