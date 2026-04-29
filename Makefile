.PHONY: help build check fmt clippy doc test deny

TEST_ARGS ?=

# Wrap every cargo invocation with `nice -n 19 ionice -c 3` so heavy
# rebuilds do not starve the desktop for I/O. Override by setting
# `CARGO=cargo` on the command line for an unwrapped run.
CARGO ?= nice -n 19 ionice -c 3 cargo

help:
	@echo "formawasm maintainer targets:"
	@echo ""
	@echo "  make build      Build all targets (lib + tests) under nice/ionice"
	@echo "  make check      Run the full local check suite (fmt + clippy + doc + test)"
	@echo "  make fmt        Check formatting (cargo fmt --check)"
	@echo "  make clippy     Run clippy with -D warnings (same as CI)"
	@echo "  make doc        Build rustdoc with -D warnings — catches broken intra-doc links"
	@echo "  make test       Run all tests"
	@echo "  make deny       Run cargo-deny license + advisory checks"

build:
	$(CARGO) build --all-targets

check: fmt clippy doc test

fmt:
	$(CARGO) fmt --check

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

# Build rustdoc with warnings as errors so dead intra-doc links and
# private-item leaks are caught before they rot. --no-deps keeps the
# scope local to this crate.
doc:
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --no-deps

test:
	$(CARGO) test $(TEST_ARGS)

deny:
	$(CARGO) deny check
