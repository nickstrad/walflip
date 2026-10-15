BIN := target/release/walflip
DIR := walflip-data
AT250 := /tmp/at250.db
AT500 := /tmp/at500.db

.PHONY: build test fuzz seed demo clean distclean

build:
	cargo build --release

# Unit, property and end-to-end tests
test:
	cargo test

# Coverage-guided fuzzing (needs nightly + cargo-fuzz); FUZZ_SECS per target
FUZZ_SECS ?= 30
fuzz:
	cargo +nightly fuzz run wal_parse -- -max_total_time=$(FUZZ_SECS)

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
