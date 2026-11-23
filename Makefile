.PHONY: target/release/jrsonnet target/release/rtk target/release/tk-compare build-rtk-quiet build-tk-compare-quiet tk-compare fmt-acceptance fmt-acceptance-corpus lint lint-all lint-ci fmt fmt-check test test-rtk check check-rtk ci ci-full help update-golden-fixtures check-golden-fixtures check-glob-truth-table go-jsonnet-checkout check-fmt-go-jsonnet-pin update-fmt-corpus check-fmt-corpus update-fmt-lexer-oracle update-fmt-node-oracle update-fmt-pass-oracle update-go-sort-truth-table check-go-sort-truth-table check-generated

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
	@echo "  update-fmt-lexer-oracle - Regenerate small lexer and string regression oracles"
	@echo "  check-fmt-lexer-oracle  - Compare small saved lexer/string oracles with pinned Go"
	@echo "  update-golden-fixtures - Regenerate golden files in test_fixtures using tk export"
	@echo "  check-golden-fixtures  - Check that golden files are up to date (requires tk)"
	@echo "  check-glob-truth-table - Run mandatory live gobwas/glob comparisons (requires Go)"
	@echo "  go-jsonnet-checkout    - Clone the pinned go-jsonnet to target/go-jsonnet (requires Go)"
	@echo "  update-fmt-corpus      - Regenerate the go-jsonnet formatter corpus (requires Go)"
	@echo "  check-fmt-corpus       - Check the formatter corpus is up to date (requires Go)"
	@echo "  check-generated        - Check every Go-generated artifact at once (requires Go)"
	@echo "  update-fmt-lexer-oracle - Regenerate the go-jsonnet token/fodder oracle (requires Go)"
	@echo "  update-fmt-node-oracle - Regenerate the go-jsonnet AST/fodder-slot oracle (requires Go)"
	@echo "  update-fmt-pass-oracle - Regenerate the go-jsonnet per-pass AST oracle (requires Go)"
	@echo "  update-go-sort-truth-table - Regenerate the Go sort.Slice permutation table (requires Go)"
	@echo "  check-go-sort-truth-table - Check the sort.Slice table is up to date (requires Go)"
	@echo "  fmt-acceptance-corpus  - Clone the pinned real-Jsonnet fmt acceptance corpus (requires git, network)"
	@echo "  fmt-acceptance         - Run the fmt acceptance gate against tk (requires tk, the corpus)"

target/release/jrsonnet:
	@cargo build --release -p jrsonnet

target/release/rtk:
	@cargo build --release -p rtk

target/release/tk-compare:
	@cargo build --release -p tk-compare

tk-compare: target/release/rtk target/release/tk-compare
	@target/release/tk-compare -- run tk-compare-in-repo.toml --jrsonnet-path=target/release/jrsonnet --rtk=target/release/rtk

lint:
	@cargo clippy -p rtk --all-targets

lint-all:
	@cargo clippy --all-targets --all-features

fmt:
	@cargo fmt --all

fmt-check:
	@cargo fmt --all -- --check

# `tests/fixtures.rs` grades the `go_jsonnet/` family — go-jsonnet's own
# `formatter/testdata/*.fmt.golden` — against GO_JSONNET_FOR_TESTS, falling back
# to `target/go-jsonnet`. That fallback lives in the test rather than here on
# purpose: putting it in this target graded the family under `make test` while a
# plain `cargo test -p rtk-jsonnetfmt` still skipped it, which is the entry
# point anyone actually uses while working. Nothing to set here as a result.
#
# This comment used to say the oracle targets leave a checkout there. **None
# did**, so the family graded 12 of 15 everywhere outside the nix devShell. See
# `go-jsonnet-checkout` below, which is the target that now makes it true; run
# it once and this and every later `cargo test` grades all fifteen.
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

# Generate golden files for test_fixtures using tk export
# Uses .golden extension to prevent accidental reformatting
GOLDEN_FIXTURES_DIR := test_fixtures/golden_envs

update-golden-fixtures: target/release/jrsonnet target/release/tk-compare
	@echo "Generating golden files for $(GOLDEN_FIXTURES_DIR)..."
	@target/release/tk-compare --jrsonnet-path $(CURDIR)/target/release/jrsonnet golden-fixtures --fixtures-dir $(GOLDEN_FIXTURES_DIR)

# Check that golden files are up to date (for CI)
check-golden-fixtures: target/release/jrsonnet target/release/tk-compare
	@echo "Checking golden files are up to date..."
	@target/release/tk-compare --jrsonnet-path $(CURDIR)/target/release/jrsonnet golden-fixtures --dry-run --fixtures-dir $(GOLDEN_FIXTURES_DIR)
	@echo "Golden files are up to date."

# Small saved regressions only. cargo test generates the large corpus oracle at runtime.
.PHONY: update-fmt-lexer-oracle check-fmt-lexer-oracle
update-fmt-lexer-oracle:
	python3 crates/rtk-jsonnetfmt/testdata/generate/lexer_oracle.py

check-fmt-lexer-oracle:
	python3 crates/rtk-jsonnetfmt/testdata/generate/lexer_oracle.py --check


# A go-jsonnet checkout at a stable path.
#
# Two things need one. The fmt corpus's second set is go-jsonnet's own root
# testdata/, whose inputs are not in this repository; and
# `crates/rtk-jsonnetfmt/tests/fixtures.rs` grades the `go_jsonnet/` fixture
# family against `formatter/testdata/*.fmt.golden`.
#
# That second one is why this target exists at all, and it is a bug fix rather
# than a convenience. fixtures.rs falls back to `target/go-jsonnet` when
# GO_JSONNET_FOR_TESTS is unset, and its comment — and the one further up this
# file, and CLAUDE.md — all said the oracle targets leave one there. **None
# did.** `update-fmt-node-oracle` is an ordinary program in the generate module
# and takes go-jsonnet from the module cache; the lexer and pass oracles clone
# into `mktemp -d` and delete it. So on every machine without the nix devShell,
# CI included, the family graded 12 of 15 and said so only in a log nobody
# reads — and two of the three missing carry answers nothing else in the suite
# has. `make go-jsonnet-checkout` is what makes that fallback true.
#
# GO_JSONNET_VERSION is defined further down, with the lexer oracle that needed
# it first.
GO_JSONNET_CHECKOUT := target/go-jsonnet
# What every recipe below resolves to: an explicit checkout wins, and failing
# that the one this file clones.
GO_JSONNET_PATH = $${GO_JSONNET_FOR_TESTS:-$(abspath $(GO_JSONNET_CHECKOUT))}

# Rust's build cache can restore an incomplete directory under target/. Check
# the contents on every invocation rather than trusting the directory's existence.
GO_JSONNET_VERSION := v0.22.0
.PHONY: $(GO_JSONNET_CHECKOUT)
$(GO_JSONNET_CHECKOUT):
	python3 crates/rtk-jsonnetfmt/testdata/generate/ast_oracle.py --checkout $(GO_JSONNET_CHECKOUT)

go-jsonnet-checkout: $(GO_JSONNET_CHECKOUT)

# The breadth corpus Phase 2 is measured against: go-jsonnet's formatter run
# over every Jsonnet file in this repository, and over go-jsonnet's own root
# testdata/. Replaces the round-trip gate the plan originally asked for, which
# go-jsonnet itself does not satisfy — its unparser renders from the fodder
# model rather than copying the source.
#
# Phase 4 added the second set. The in-repo set stays where it is because the
# node, pass and lexer oracles mirror its manifest file for file; the external
# set is one self-contained JSON carrying inputs as well as answers, so its
# count does not depend on a checkout being present. See the package comment on
# $(FMT_GENERATE_DIR)/main.go.
FMT_GENERATE_DIR := crates/rtk-jsonnetfmt/testdata/generate
FMT_CORPUS_DIR := crates/rtk-jsonnetfmt/testdata/corpus
FMT_EXTERNAL_CORPUS := crates/rtk-jsonnetfmt/testdata/go-jsonnet-corpus.json

# The two sets have to answer for the same go-jsonnet: one takes it from the
# module cache via go.mod, the other from the cloned checkout. Nothing else
# would notice them drifting apart.
check-fmt-go-jsonnet-pin:
	@grep -qx "require github.com/google/go-jsonnet $(GO_JSONNET_VERSION)" \
		$(FMT_GENERATE_DIR)/go.mod || { \
		echo "$(FMT_GENERATE_DIR)/go.mod does not require go-jsonnet $(GO_JSONNET_VERSION),"; \
		echo "which is the version this file clones. The two corpus sets would answer"; \
		echo "for different go-jsonnets. Reconcile GO_JSONNET_VERSION and go.mod."; \
		exit 1; \
	}

update-fmt-corpus: check-fmt-go-jsonnet-pin $(GO_JSONNET_CHECKOUT)
	@echo "Regenerating $(FMT_CORPUS_DIR) and $(FMT_EXTERNAL_CORPUS) with go-jsonnet's formatter..."
	@checkout="$(GO_JSONNET_PATH)"; \
		cd $(FMT_GENERATE_DIR) && go run . \
			$(CURDIR) $(CURDIR)/$(FMT_CORPUS_DIR) \
			"$$checkout" $(CURDIR)/$(FMT_EXTERNAL_CORPUS)
	@echo "Corpus regenerated. Review the diff, and update the per-set counts in"
	@echo "crates/rtk-jsonnetfmt/testdata/corpus-baseline.toml if either moved."

check-fmt-corpus: check-fmt-go-jsonnet-pin $(GO_JSONNET_CHECKOUT)
	@test -f $(FMT_CORPUS_DIR)/manifest.json || { \
		echo "$(FMT_CORPUS_DIR) is missing; run 'make update-fmt-corpus' (requires Go)"; \
		exit 1; \
	}
	@test -f $(FMT_EXTERNAL_CORPUS) || { \
		echo "$(FMT_EXTERNAL_CORPUS) is missing; run 'make update-fmt-corpus' (requires Go)"; \
		exit 1; \
	}
	@set -e; \
		checkout="$(GO_JSONNET_PATH)"; \
		tmp=$$(mktemp -d); \
		(cd $(FMT_GENERATE_DIR) && go run . \
			$(CURDIR) "$$tmp/corpus" "$$checkout" "$$tmp/go-jsonnet-corpus.json"); \
		diff -ru $(FMT_CORPUS_DIR) "$$tmp/corpus"; \
		diff -u $(FMT_EXTERNAL_CORPUS) "$$tmp/go-jsonnet-corpus.json"; \
		rm -rf "$$tmp"
	@echo "Corpus is up to date."

# The truth table crates/rtk-jsonnetfmt/src/go_sort.rs is graded against: the
# permutation Go's own `sort.Slice` produces.
#
# `SortImports` sorts imports by a key two of them can share, and `sort.Slice`
# is documented as *not* stable — so which of two equal-path imports comes
# first is decided by pdqsort's pivot choices, and the pass oracle measured
# that it inverts ties from n = 13 upwards. Neither `sort_by` nor
# `sort_unstable_by` gives that answer, hence a port; and a permutation
# produced by pdqsort is the least plausible thing in this repository to
# derive by hand, hence a table.
#
# Needs no staged checkout and no dependency: `sort` is an ordinary
# standard-library package, so this is a plain program in the generate module.
# The table records the Go version that produced it, because that is what
# would have to change for the answers to move.
GO_SORT_TRUTH_TABLE := crates/rtk-jsonnetfmt/testdata/go-sort-truth-table.json

update-go-sort-truth-table:
	@echo "Regenerating $(GO_SORT_TRUTH_TABLE) with Go's sort.Slice..."
	@cd $(FMT_GENERATE_DIR) && go run ./sortdump $(CURDIR)/$(GO_SORT_TRUTH_TABLE)
	@echo "Truth table regenerated. Review the diff before committing: a change"
	@echo "to goVersion together with changed permutations means tk fmt's output"
	@echo "has moved and src/go_sort.rs needs revisiting."

check-go-sort-truth-table:
	@test -f $(GO_SORT_TRUTH_TABLE) || { \
		echo "$(GO_SORT_TRUTH_TABLE) is missing; run 'make update-go-sort-truth-table' (requires Go)"; \
		exit 1; \
	}
	@tmp=$$(mktemp) && \
		(cd $(FMT_GENERATE_DIR) && go run ./sortdump $$tmp) && \
		diff -u $(GO_SORT_TRUTH_TABLE) $$tmp && rm -f $$tmp
	@echo "Truth table is up to date."

# Everything whose committed answer is produced by a Go library, checked at
# once. This is the local equivalent of CI's `check-generated` job, which runs
# the three separately so a drifted table is its own red check rather than one
# hidden behind whichever check happened to run first.
#
# Nothing ran any of these in CI before Phase 4: `cargo test --all` grades the
# formatter against the *committed* corpus, so correctness was covered, but
# whether the committed corpus and the two truth tables still equal what the Go
# libraries produce was checked by nobody. A go-jsonnet bump or a hand edit to a
# golden drifted silently with every fmt test green.
check-generated: check-fmt-corpus check-go-sort-truth-table check-glob-truth-table check-fmt-lexer-oracle check-fmt-node-oracle check-fmt-pass-oracle check-fmt-parse-errors
	@echo "All generated artifacts are up to date."

# Phase 5's acceptance gate: `rtk fmt` against `tk fmt` over real Grafana
# Jsonnet, vendor included.
#
# Deliberately **not** part of `check`, `ci` or `check-generated`. It needs
# network access to clone several hundred megabytes and a `tk` on PATH, so as a
# pull-request gate it would fail for reasons that have nothing to do with the
# change under review. `.github/workflows/fmt-acceptance.yaml` runs it weekly
# and files one issue on divergence, the way tk-latest.yaml does.
#
# What it measures and why the corpus is what it is: fmt-acceptance.toml.
# Everything it prints is printed on success as well as failure — the counts are
# the output of this gate, not a debugging aid.
FMT_ACCEPTANCE_CONFIG := fmt-acceptance.toml
FMT_ACCEPTANCE_CORPUS := target/fmt-acceptance-corpus

fmt-acceptance-corpus:
	@scripts/fmt-acceptance-corpus.sh $(FMT_ACCEPTANCE_CONFIG) $(FMT_ACCEPTANCE_CORPUS)

# `--rtk` is absolute because the already-formatted gate runs `rtk fmt` with the
# staging tree as its working directory, where a relative path would not
# resolve. `tk` comes from PATH, pinned by .github/actions/install-tk in CI.
fmt-acceptance: target/release/rtk target/release/tk-compare
	@target/release/tk-compare fmt-acceptance \
		--config $(FMT_ACCEPTANCE_CONFIG) \
		--corpus-dir $(FMT_ACCEPTANCE_CORPUS) \
		--rtk=$(CURDIR)/target/release/rtk

check-glob-truth-table:
	cargo test -p rtk-gobwas-glob --test truth_table

# Large AST/pass answers are generated by every parity run. Only small
# regressions are saved, and these targets verify their reproducibility.
.PHONY: update-fmt-node-oracle check-fmt-node-oracle update-fmt-pass-oracle check-fmt-pass-oracle update-fmt-parse-errors check-fmt-parse-errors test-fmt-oracles
update-fmt-node-oracle:
	python3 crates/rtk-jsonnetfmt/testdata/generate/ast_oracle.py --kind node
check-fmt-node-oracle:
	python3 crates/rtk-jsonnetfmt/testdata/generate/ast_oracle.py --kind node --check
update-fmt-pass-oracle:
	python3 crates/rtk-jsonnetfmt/testdata/generate/ast_oracle.py --kind pass
check-fmt-pass-oracle:
	python3 crates/rtk-jsonnetfmt/testdata/generate/ast_oracle.py --kind pass --check
update-fmt-parse-errors:
	python3 crates/rtk-jsonnetfmt/testdata/generate/ast_oracle.py --kind parse-error
check-fmt-parse-errors:
	python3 crates/rtk-jsonnetfmt/testdata/generate/ast_oracle.py --kind parse-error --check
test-fmt-oracles:
	cargo test -p rtk-jsonnetfmt --test node_oracle --test node_parity --test pass_parity --test lexer_oracle --test oracle_requirements

.PHONY: check-fmt-acceptance-regressions
check-fmt-acceptance-regressions: target/release/rtk target/release/tk-compare
	python3 scripts/check_fmt_acceptance.py --compare target/release/tk-compare --rtk target/release/rtk
	$${PYTHON:-python3} -m unittest discover -s scripts -p 'test_fmt_acceptance_corpus.py'
