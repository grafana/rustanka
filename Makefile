.PHONY: build-rtk build-tk-compare build-rtk-quiet build-tk-compare-quiet tk-compare-grafana lint lint-all lint-ci fmt fmt-check test test-rtk check check-rtk ci ci-full help

.DEFAULT_GOAL := help

help:
	@echo "Available targets:"
	@echo "  build-rtk              - Build the rtk binary in release mode"
	@echo "  build-tk-compare       - Build the tk-compare binary in release mode"
	@echo "  lint                   - Run clippy linter on rtk"
	@echo "  lint-all               - Run clippy linter on all packages"
	@echo "  lint-ci                - Run clippy with CI settings (-D warnings)"
	@echo "  fmt                    - Format code with rustfmt"
	@echo "  fmt-check              - Check code formatting (no changes)"
	@echo "  test                   - Run all tests"
	@echo "  test-rtk               - Run rtk tests only"
	@echo "  check                  - Run all checks (fmt-check, lint, test)"
	@echo "  check-rtk              - Run rtk checks only (fmt-check, lint, test-rtk)"
	@echo "  ci                     - Run CI checks locally (fmt, lint-ci, test-rtk)"
	@echo "  ci-full                - Run full CI checks (fmt, lint-ci, all tests)"
	@echo "  tk-compare-grafana     - Run tk-compare against Grafana deployment_tools"
	@echo ""
	@echo "Environment variables for tk-compare-grafana:"
	@echo "  DEPLOYMENT_TOOLS_PATH  - Path to grafana/deployment_tools repository (required)"
	@echo "  TK_PATH                - Path to tk executable (required)"


build-rtk:
	@cargo build --release -p rtk

build-tk-compare:
	@cargo build --release -p tk-compare

tk-compare-grafana:
	make build-rtk > /dev/null 2>&1
	make build-tk-compare > /dev/null 2>&1
	@if [ -z "$(DEPLOYMENT_TOOLS_PATH)" ]; then \
		echo "Error: DEPLOYMENT_TOOLS_PATH is not set"; \
		echo "Usage: make tk-compare-grafana DEPLOYMENT_TOOLS_PATH=/path/to/deployment_tools TK_PATH=/path/to/tk"; \
		exit 1; \
	fi
	@if [ -z "$(TK_PATH)" ]; then \
		echo "Error: TK_PATH is not set"; \
		echo "Usage: make tk-compare-grafana DEPLOYMENT_TOOLS_PATH=/path/to/deployment_tools TK_PATH=/path/to/tk"; \
		exit 1; \
	fi
	@if [ ! -d "$(DEPLOYMENT_TOOLS_PATH)" ]; then \
		echo "Error: DEPLOYMENT_TOOLS_PATH does not exist: $(DEPLOYMENT_TOOLS_PATH)"; \
		exit 1; \
	fi
	@if [ ! -x "$(TK_PATH)" ]; then \
		echo "Error: TK_PATH is not executable: $(TK_PATH)"; \
		exit 1; \
	fi
	DEPLOYMENT_TOOLS_PATH=$(DEPLOYMENT_TOOLS_PATH) TK_PATH=$(TK_PATH) ./target/release/tk-compare tk-compare-grafana.toml

lint:
	@cargo clippy -p rtk --all-targets

lint-all:
	@cargo clippy --all-targets --all-features

fmt:
	@cargo fmt --all

fmt-check:
	@cargo fmt --all -- --check

test:
	@cargo test --all

test-rtk:
	@cargo test -p rtk

check: fmt-check lint test
	@echo "All checks passed!"

check-rtk: fmt-check lint test-rtk
	@echo "All rtk checks passed!"

# CI targets - match GitHub Actions settings
lint-ci:
	RUSTFLAGS="-D warnings" cargo clippy --all-targets

ci: fmt-check lint-ci test-rtk
	@echo "All CI checks passed!"

ci-full: fmt-check lint-ci test
	@echo "All CI checks passed (full)!"
