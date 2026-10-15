#!/usr/bin/env bash
# Seeds the 'processes' database with exactly 500 changes: 250 INSERT, 150 UPDATE, 100 DELETE.
# The data is the process table, i.e. what ps(1) reads from the kernel.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${WALFLIP:-target/release/walflip}
[[ -x $BIN ]] || cargo build --release
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

ps -axo pid=,ppid=,pcpu=,rss=,stat=,comm= > "$tmp/a.txt"
sleep 1
ps -axo pid=,ppid=,pcpu=,rss=,stat=,comm= > "$tmp/b.txt"
# Test hook: SEED_FAKE_PS_ROWS=N truncates both ps samples to N rows to exercise the padding path.
if [[ -n ${SEED_FAKE_PS_ROWS:-} ]]; then
  head -n "$SEED_FAKE_PS_ROWS" "$tmp/a.txt" > "$tmp/a.t" && mv "$tmp/a.t" "$tmp/a.txt"
  head -n "$SEED_FAKE_PS_ROWS" "$tmp/b.txt" > "$tmp/b.t" && mv "$tmp/b.t" "$tmp/b.txt"
fi

# Small hosts (CI containers) can have fewer than 250 processes. Pad with synthetic rows so the
# output is deterministic: pids from 90000000 up cannot collide with real pids (pid_max <= 2^22),
# and the second sample differs in rss and cpu so each UPDATE changes the row.
have=$(($(wc -l < "$tmp/a.txt")))
if (( have < 250 )); then
  echo "seed.sh: only $have processes from ps; padding with $((250 - have)) synthetic rows" >&2
  for (( i = have + 1; i <= 250; i++ )); do
    pid=$((90000000 + i)); rss=$((2048 + i * 16))
    echo "$pid 1 0.$((i % 10)) $rss S fakeproc-$i" >> "$tmp/a.txt"
    echo "$pid 1 1.$((i % 10)) $((rss + 4096)) S fakeproc-$i" >> "$tmp/b.txt"
  done
fi

# Pass 1 (b.txt): remember the second sample. Pass 2 (a.txt): first 250 rows become INSERTs;
# END emits 150 UPDATEs (rows 1..150) interleaved with 100 DELETEs (rows 151..250).
awk -v N=250 '
function q(s) { gsub(/\047/, "\047\047", s); return "\047" s "\047" }
function upd(k,   p, rss, cpu) {
  p = pid[k]
  # SQLite skips the write when a row is set to identical values, so force a real change.
  rss = (p in b_rss && b_rss[p] != a_rss[k]) ? b_rss[p] : a_rss[k] + 1024
  cpu = (p in b_cpu) ? b_cpu[p] : 0
  printf "UPDATE processes SET cpu_pct=%s, rss_kb=%d WHERE pid=%d;\n", cpu, rss, p
}
FNR == NR { b_rss[$1] = $4; b_cpu[$1] = $3; next }
FNR <= N {
  name = $6; for (i = 7; i <= NF; i++) name = name " " $i
  sub(/.*\//, "", name)
  pid[FNR] = $1; a_rss[FNR] = $4
  printf "INSERT INTO processes(pid,ppid,name,state,cpu_pct,rss_kb) VALUES(%d,%d,%s,%s,%s,%d);\n", $1, $2, q(name), q($5), $3, $4
}
END {
  for (k = 1; k <= 100; k++) { upd(k); printf "DELETE FROM processes WHERE pid=%d;\n", pid[150 + k] }
  for (k = 101; k <= 150; k++) upd(k)
}' "$tmp/b.txt" "$tmp/a.txt" > "$tmp/changes.sql"

echo "generated $(wc -l < "$tmp/changes.sql") statements"
DB=${WALFLIP_DB:-processes}
export WALFLIP_DB=$DB
"$BIN" drop "$DB" 2>/dev/null || true
"$BIN" create "$DB" --schema examples/processes.sql
"$BIN" exec - < "$tmp/changes.sql"
"$BIN" status | tee "$tmp/status.txt"
# Every statement must have produced a commit; SQLite skips writes that change nothing.
grep -qx 'commits   : 500' "$tmp/status.txt" || { echo "expected 500 commits" >&2; exit 1; }
cat <<'EOF'

try (database "processes", or set WALFLIP_DB):
  walflip list
  walflip --db processes log --limit 10      # the 10 most recent; --head for the first 10, --all for all
  walflip --db processes clone -n 250 -o /tmp/at250.db    # inserts only: 250 rows
  walflip --db processes clone -n 500 -o /tmp/at500.db    # everything: 150 rows
EOF
