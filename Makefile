.PHONY: help check fmt clippy doc test deny

TEST_ARGS ?=

help:
	@echo "formawasm maintainer targets:"
	@echo ""
	@echo "  make check      Run the full local check suite (fmt + clippy + doc + test)"
	@echo "  make fmt        Check formatting (cargo fmt --check)"
	@echo "  make clippy     Run clippy with -D warnings (same as CI)"
	@echo "  make doc        Build rustdoc with -D warnings — catches broken intra-doc links"
	@echo "  make test       Run all tests"
	@echo "  make deny       Run cargo-deny license + advisory checks"

check: fmt clippy doc test

fmt:
	cargo fmt --check

clippy:
	cargo clippy --all-targets -- -D warnings

# Build rustdoc with warnings as errors so dead intra-doc links and
# private-item leaks are caught before they rot. --no-deps keeps the
# scope local to this crate.
doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

test:
	cargo test $(TEST_ARGS)

deny:
	cargo deny check
