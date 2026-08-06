# Convenience wrapper around cargo. `make check` runs the same gates as CI
# (.github/workflows/ci.yml): fmt --check, clippy -D warnings, build, test.

.PHONY: build release test fmt fmt-check lint check install bench clean

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --all-targets -- -D warnings

check: fmt-check lint build test

install:
	cargo install --path .

bench:
	cargo bench

clean:
	cargo clean
