BIN := target/release/walflip
DIR := walflip-data
AT250 := /tmp/at250.db
AT500 := /tmp/at500.db

.PHONY: build test fuzz fuzz-replay seed demo clean distclean

build:
	cargo build --release

# Unit, property and end-to-end tests
test:
	cargo test --features testing

# Coverage-guided fuzzing (needs nightly + cargo-fuzz); FUZZ_SECS per target
FUZZ_SECS ?= 30
FUZZ_TARGETS := wal_parse wal_differential replay_apply store_capture statements_split exec_sql
fuzz:
	@for t in $(FUZZ_TARGETS); do \
	  extra=""; [ $$t = wal_parse ] && extra="-max_len=70000"; [ $$t = exec_sql ] && extra="-timeout=30"; \
	  echo "== $$t =="; \
	  cargo +nightly fuzz run $$t -- -max_total_time=$(FUZZ_SECS) -rss_limit_mb=1024 -dict=fuzz/wal.dict $$extra || exit 1; \
	done

# Replay committed corpora once (regression check)
fuzz-replay:
	@for t in $(FUZZ_TARGETS); do \
	  extra=""; [ $$t = wal_parse ] && extra="-max_len=70000"; [ $$t = exec_sql ] && extra="-timeout=30"; \
	  cargo +nightly fuzz run $$t fuzz/corpus/$$t -- -runs=0 -rss_limit_mb=1024 -dict=fuzz/wal.dict $$extra || exit 1; \
	done

# Create walflip-data/processes/ and seed it with 500 changes
seed: build
	WALFLIP=$(BIN) scripts/seed.sh

# Seed, then clone at 250 and 500 commits into /tmp
demo: seed
	rm -f $(AT250) $(AT500)
	$(BIN) --db processes clone -n 250 -o $(AT250)
	$(BIN) --db processes clone -n 500 -o $(AT500)

# Remove example data and clones, keep the build
clean:
	rm -rf $(DIR) $(AT250) $(AT500)

# Also remove build artifacts
distclean: clean
	cargo clean
