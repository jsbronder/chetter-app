.PHONY: default check-format lint test

default:
	@cargo build

check-format:
	@cargo fmt --check

lint: check-format
	@cargo clippy --no-deps --all-targets -- --deny warnings

test:
	@cargo test
