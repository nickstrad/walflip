#!/usr/bin/env bash
# Regenerates the seed part of fuzz/corpus/{wal_parse,store_capture}:
#   - real WAL files produced by walflip exec / SQLite (tests/fuzz_seed.rs, Harness::wal_snapshot)
#   - encoder-generated WALs (both byte orders, page sizes 512..65536, 0/1/5 commits, stale tails)
# Seeds are minimised with `cargo fuzz cmin` and copied in as seed-* files. Other files in those
# directories (fuzzer finds, hand-written seeds) are left alone.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

TARGETS=(wal_parse store_capture)
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

WALFLIP_SEED_OUT="$out" cargo test --features testing --test fuzz_seed -- --ignored --nocapture

for t in "${TARGETS[@]}"; do
  [[ -d $out/$t ]] || { echo "generator produced no $t seeds" >&2; exit 1; }
  echo "raw seeds for $t: $(ls "$out/$t" | wc -l | tr -d ' ')"
  cargo +nightly fuzz cmin "$t" "$out/$t"
  dest=fuzz/corpus/$t
  mkdir -p "$dest"
  rm -f "$dest"/seed-*
  for f in "$out/$t"/*; do
    cp "$f" "$dest/seed-$(basename "$f")"
  done
  echo "committed seeds for $t: $(ls "$dest" | grep -c '^seed-') ($(du -sh "$dest" | cut -f1))"
done

total=$(du -sk fuzz/corpus | cut -f1)
echo "fuzz/corpus total: ${total}K"
(( total < 2048 )) || { echo "fuzz/corpus is over 2 MB" >&2; exit 1; }

cat <<'EOF'

replay the committed corpus (must pass):
  cargo +nightly fuzz run wal_parse fuzz/corpus/wal_parse -- -runs=0 -max_len=70000
  cargo +nightly fuzz run store_capture fuzz/corpus/store_capture -- -runs=0
EOF
