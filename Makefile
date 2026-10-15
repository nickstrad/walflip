BIN := target/release/walflip
DIR := walflip-data
AT250 := /tmp/at250.db
AT500 := /tmp/at500.db

.PHONY: build seed demo clean distclean

build:
	cargo build --release

# Create walflip-data/ and seed it with 500 changes
seed: build
	WALFLIP=$(BIN) scripts/seed.sh

# Seed, then clone at 250 and 500 commits into /tmp
demo: seed
	rm -f $(AT250) $(AT500)
	$(BIN) clone -n 250 -o $(AT250)
	$(BIN) clone -n 500 -o $(AT500)

# Remove example data and clones, keep the build
clean:
	rm -rf $(DIR) $(AT250) $(AT500)

# Also remove build artifacts
distclean: clean
	cargo clean
