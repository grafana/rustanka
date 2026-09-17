.PHONY: target/release/jrsonnet target/release/rtk target/release/tk-compare build-rtk-quiet build-tk-compare-quiet tk-compare-grafana lint lint-all lint-ci fmt fmt-check test test-rtk check check-rtk ci ci-full help update-golden-fixtures check-golden-fixtures update-glob-truth-table check-glob-truth-table go-jsonnet-checkout check-fmt-go-jsonnet-pin update-fmt-corpus check-fmt-corpus update-fmt-lexer-oracle update-fmt-node-oracle update-fmt-pass-oracle update-go-sort-truth-table check-go-sort-truth-table check-generated

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
	@echo "  update-golden-fixtures - Regenerate golden files in test_fixtures using tk export"
	@echo "  check-golden-fixtures  - Check that golden files are up to date (requires tk)"
	@echo "  update-glob-truth-table - Regenerate the gobwas/glob truth table (requires Go)"
	@echo "  check-glob-truth-table - Check the gobwas/glob truth table is up to date (requires Go)"
	@echo "  go-jsonnet-checkout    - Clone the pinned go-jsonnet to target/go-jsonnet (requires Go)"
	@echo "  update-fmt-corpus      - Regenerate the go-jsonnet formatter corpus (requires Go)"
	@echo "  check-fmt-corpus       - Check the formatter corpus is up to date (requires Go)"
	@echo "  check-generated        - Check every Go-generated artifact at once (requires Go)"
	@echo "  update-fmt-lexer-oracle - Regenerate the go-jsonnet token/fodder oracle (requires Go)"
	@echo "  update-fmt-node-oracle - Regenerate the go-jsonnet AST/fodder-slot oracle (requires Go)"
	@echo "  update-fmt-pass-oracle - Regenerate the go-jsonnet per-pass AST oracle (requires Go)"
	@echo "  update-go-sort-truth-table - Regenerate the Go sort.Slice permutation table (requires Go)"
	@echo "  check-go-sort-truth-table - Check the sort.Slice table is up to date (requires Go)"

target/release/jrsonnet:
	@cargo build --release -p jrsonnet

target/release/rtk:
	@cargo build --release -p rtk

target/release/tk-compare:
	@cargo build --release -p tk-compare

tk-compare: target/release/rtk target/release/tk-compare
	@target/release/tk-compare -- run tk-compare-grafana.toml --jrsonnet-path=target/release/jrsonnet --rtk=target/release/rtk

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

# The oracle rtk-gobwas-glob is graded against, generated by the exact library
# and version tk links. Read the table, not gobwas' documentation: the two
# disagree about separators, and it is the table that decides which files
# `rtk fmt` and `rtk lint` touch.
GLOB_GENERATE_DIR := crates/rtk-gobwas-glob/testdata/generate
GLOB_TRUTH_TABLE := crates/rtk-gobwas-glob/testdata/gobwas-glob-v0.2.3.json

update-glob-truth-table:
	@echo "Regenerating $(GLOB_TRUTH_TABLE) with gobwas/glob..."
	@cd $(GLOB_GENERATE_DIR) && go mod tidy && go run . > $(CURDIR)/$(GLOB_TRUTH_TABLE)
	@echo "Truth table regenerated. Review the diff before committing."

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
GO_JSONNET_PATH = $${GO_JSONNET_FOR_TESTS:-$(CURDIR)/$(GO_JSONNET_CHECKOUT)}

# A real directory target rather than a phony one, so it is cloned once and
# then left alone.
$(GO_JSONNET_CHECKOUT):
	@if [ -n "$${GO_JSONNET_FOR_TESTS:-}" ]; then \
		echo "GO_JSONNET_FOR_TESTS is set to $$GO_JSONNET_FOR_TESTS; not cloning"; \
	else \
		echo "cloning go-jsonnet $(GO_JSONNET_VERSION) into $@..."; \
		mkdir -p $(dir $@); \
		git clone --quiet --depth 1 --branch $(GO_JSONNET_VERSION) \
			https://github.com/google/go-jsonnet $@; \
	fi

go-jsonnet-checkout: $(GO_JSONNET_CHECKOUT)
	@checkout="$(GO_JSONNET_PATH)"; \
	test -d "$$checkout/formatter/testdata" || { \
		echo "$$checkout does not look like a go-jsonnet checkout"; exit 1; \
	}; \
	echo "go-jsonnet checkout ready at $$checkout"

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
		cd $(FMT_GENERATE_DIR) && go mod tidy && go run . \
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
		(cd $(FMT_GENERATE_DIR) && go mod tidy && go run . \
			$(CURDIR) "$$tmp/corpus" "$$checkout" "$$tmp/go-jsonnet-corpus.json"); \
		diff -ru $(FMT_CORPUS_DIR) "$$tmp/corpus"; \
		diff -u $(FMT_EXTERNAL_CORPUS) "$$tmp/go-jsonnet-corpus.json"; \
		rm -rf "$$tmp"
	@echo "Corpus is up to date."

# The lexer oracle: the tokens and fodder go-jsonnet's own lexer produces.
#
# Fodder is only observable from inside go-jsonnet — `internal/parser` is an
# internal package and `token`'s fields are unexported — so this stages a
# _test.go into a checkout and runs it there. Set GO_JSONNET_FOR_TESTS to an
# existing checkout (the nix devShell already does) or this clones the pinned
# version. Worth the staging: everything downstream of the lexer inherits its
# mistakes, and the corpus only sees them once they have reached text.
FMT_LEXER_ORACLE := crates/rtk-jsonnetfmt/testdata/lexer-oracle.json
# Snippets cover what the corpus cannot reach: it holds no verbatim string and
# provokes no lexer error, so without these the error messages would be pinned
# by expectations derived by hand rather than taken from go-jsonnet.
FMT_LEXER_SNIPPETS := crates/rtk-jsonnetfmt/testdata/lexer-snippets.json
FMT_LEXER_SNIPPET_ORACLE := crates/rtk-jsonnetfmt/testdata/lexer-snippet-oracle.json
GO_JSONNET_VERSION := v0.22.0

update-fmt-lexer-oracle:
	@test -f $(FMT_CORPUS_DIR)/manifest.json || { \
		echo "generate the corpus first: make update-fmt-corpus"; exit 1; \
	}
	@set -e; \
	checkout="$${GO_JSONNET_FOR_TESTS:-}"; \
	staged=""; \
	if [ -z "$$checkout" ]; then \
		staged=$$(mktemp -d); \
		echo "cloning go-jsonnet $(GO_JSONNET_VERSION) into $$staged..."; \
		git clone --quiet --depth 1 --branch $(GO_JSONNET_VERSION) \
			https://github.com/google/go-jsonnet "$$staged/go-jsonnet"; \
		checkout="$$staged/go-jsonnet"; \
	else \
		echo "using GO_JSONNET_FOR_TESTS at $$checkout"; \
		staged=$$(mktemp -d); \
		cp -R "$$checkout" "$$staged/go-jsonnet"; \
		chmod -R u+w "$$staged/go-jsonnet"; \
		checkout="$$staged/go-jsonnet"; \
	fi; \
	cp $(FMT_GENERATE_DIR)/fodderdump/fodderdump_test.go \
		"$$checkout/internal/parser/fodderdump_test.go"; \
	python3 -c "import json,sys; \
		m=json.load(open('$(FMT_CORPUS_DIR)/manifest.json')); \
		sys.stdout.write(''.join(e['source']+chr(10) for e in m['entries']))" \
		> "$$staged/sources.txt"; \
	( cd "$$checkout" && \
		FODDERDUMP_ROOT=$(CURDIR) \
		FODDERDUMP_SOURCES="$$staged/sources.txt" \
		FODDERDUMP_OUT=$(CURDIR)/$(FMT_LEXER_ORACLE) \
		go test -count=1 -run TestDumpFodder ./internal/parser ); \
	( cd "$$checkout" && \
		FODDERDUMP_SNIPPETS=$(CURDIR)/$(FMT_LEXER_SNIPPETS) \
		FODDERDUMP_OUT=$(CURDIR)/$(FMT_LEXER_SNIPPET_ORACLE) \
		go test -count=1 -run TestDumpFodder ./internal/parser ); \
	rm -rf "$$staged"
	@echo "Wrote $(FMT_LEXER_ORACLE) and $(FMT_LEXER_SNIPPET_ORACLE)."

# The node oracle: the AST go-jsonnet's parser produces, with every named
# fodder slot of every node.
#
# The parser is written against this rather than against a reading of Go's
# source, for the reason the lexer was: of 16 lexer expectations derived by
# reading, 2 were wrong, and both were about fodder the model *composes* rather
# than reads. A parser composes fodder at nearly every node. The text-level
# corpus cannot locate one of those — a misplaced CommaFodder surfaces as a
# whitespace diff hundreds of lines away — and this names the slot.
#
# Unlike the lexer oracle this needs no staged checkout and no clone: every
# fodder slot is an exported field of package `ast`, and
# `formatter.SnippetToRawAST` is the same public entry point `Format` calls. So
# it is an ordinary program in the generate module, pinned to the same
# go-jsonnet the corpus came from.
FMT_NODE_ORACLE := crates/rtk-jsonnetfmt/testdata/node-oracle.json
# Snippets cover what the corpus cannot: it holds no `@"..."` at all, and one
# file each with tailstrict or importbin, so those slots would otherwise be
# graded by nothing.
FMT_NODE_SNIPPETS := crates/rtk-jsonnetfmt/testdata/node-snippets.json
FMT_NODE_SNIPPET_ORACLE := crates/rtk-jsonnetfmt/testdata/node-snippet-oracle.json
# Malformed input, dumped by the same program into a third answer file.
#
# It cannot live in $(FMT_NODE_SNIPPETS): `node_oracle.rs` asserts go-jsonnet
# refuses *none* of those, because a snippet it refuses pins no fodder slot.
# These are the opposite — every one must be refused, and what is graded is the
# refusal. `nodedump` needs no change for it; `dumpOne` already records
# `err.Error()`, which is `"<loc> <msg>"`, so the **location** is graded too.
#
# That is the gap this closes. Of go-jsonnet's 29 distinct parser-error
# templates, 26 had no graded location at all: `src/parser.rs`'s unit tests
# assert the message body with `ends_with` and defer positions to the oracle,
# and the node and pass snippet oracles contain zero error cells. A parse error
# aborts a whole `tk fmt` run, so the text and the position are both contract.
FMT_PARSE_ERROR_SNIPPETS := crates/rtk-jsonnetfmt/testdata/parse-error-snippets.json
FMT_PARSE_ERROR_ORACLE := crates/rtk-jsonnetfmt/testdata/parse-error-snippet-oracle.json

update-fmt-node-oracle:
	@test -f $(FMT_CORPUS_DIR)/manifest.json || { \
		echo "generate the corpus first: make update-fmt-corpus"; exit 1; \
	}
	@cd $(FMT_GENERATE_DIR) && go mod tidy && \
		go run ./nodedump files $(CURDIR) \
			$(CURDIR)/$(FMT_CORPUS_DIR)/manifest.json \
			$(CURDIR)/$(FMT_NODE_ORACLE)
	@cd $(FMT_GENERATE_DIR) && \
		go run ./nodedump snippets \
			$(CURDIR)/$(FMT_NODE_SNIPPETS) \
			$(CURDIR)/$(FMT_NODE_SNIPPET_ORACLE)
	@cd $(FMT_GENERATE_DIR) && \
		go run ./nodedump snippets \
			$(CURDIR)/$(FMT_PARSE_ERROR_SNIPPETS) \
			$(CURDIR)/$(FMT_PARSE_ERROR_ORACLE)
	@echo "Wrote $(FMT_NODE_ORACLE), $(FMT_NODE_SNIPPET_ORACLE) and"
	@echo "$(FMT_PARSE_ERROR_ORACLE)."

# The pass oracle: the AST each formatter pass leaves behind.
#
# The node oracle grades the parser and the corpus grades the whole pipeline;
# between them sits the question this answers — what does one pass do? The
# corpus cannot answer it. Of the 138 files it holds, `PrettyFieldNames`
# changes two and `FixTrailingCommas` and `NoRedundantSliceColon` change
# **none**, because Jsonnet already formatted by `tk fmt` gives a pass nothing
# to do. Neither can `Options`: three passes have a flag, the rest are
# unconditional.
#
# So this one is staged, like the lexer oracle and for the same kind of reason.
# `internal/formatter` may only be imported from inside go-jsonnet's own
# module, so the dumper is copied into a checkout as `rtkpassdump/` and run
# there — a `package main` inside the module, which needs no `_test.go` trick.
# nodedump/dump.go goes with it, so both oracles speak one notation.
#
# Each pass runs on its own fresh parse rather than on the pipeline's
# accumulated state: that is what lets a pass be graded before the passes
# ahead of it in `FormatNode` exist, which is the whole point of landing them
# one at a time. See crates/rtk-jsonnetfmt/testdata/generate/_staged/passdump.go.
FMT_PASS_ORACLE := crates/rtk-jsonnetfmt/testdata/pass-oracle.json
# Snippets are where this oracle earns its keep: they are the only grading two
# of the three Phase 2b passes get at all.
FMT_PASS_SNIPPETS := crates/rtk-jsonnetfmt/testdata/pass-snippets.json
FMT_PASS_SNIPPET_ORACLE := crates/rtk-jsonnetfmt/testdata/pass-snippet-oracle.json

update-fmt-pass-oracle:
	@test -f $(FMT_CORPUS_DIR)/manifest.json || { \
		echo "generate the corpus first: make update-fmt-corpus"; exit 1; \
	}
	@set -e; \
	checkout="$${GO_JSONNET_FOR_TESTS:-}"; \
	staged=$$(mktemp -d); \
	if [ -z "$$checkout" ]; then \
		echo "cloning go-jsonnet $(GO_JSONNET_VERSION) into $$staged..."; \
		git clone --quiet --depth 1 --branch $(GO_JSONNET_VERSION) \
			https://github.com/google/go-jsonnet "$$staged/go-jsonnet"; \
	else \
		echo "using GO_JSONNET_FOR_TESTS at $$checkout"; \
		cp -R "$$checkout" "$$staged/go-jsonnet"; \
		chmod -R u+w "$$staged/go-jsonnet"; \
	fi; \
	checkout="$$staged/go-jsonnet"; \
	mkdir -p "$$checkout/rtkpassdump"; \
	cp $(FMT_GENERATE_DIR)/_staged/passdump.go \
		$(FMT_GENERATE_DIR)/nodedump/dump.go \
		"$$checkout/rtkpassdump/"; \
	( cd "$$checkout" && \
		go run ./rtkpassdump files $(CURDIR) \
			$(CURDIR)/$(FMT_CORPUS_DIR)/manifest.json \
			$(CURDIR)/$(FMT_PASS_ORACLE) ); \
	( cd "$$checkout" && \
		go run ./rtkpassdump snippets \
			$(CURDIR)/$(FMT_PASS_SNIPPETS) \
			$(CURDIR)/$(FMT_PASS_SNIPPET_ORACLE) ); \
	rm -rf "$$staged"
	@echo "Wrote $(FMT_PASS_ORACLE) and $(FMT_PASS_SNIPPET_ORACLE)."

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
check-generated: check-fmt-corpus check-go-sort-truth-table check-glob-truth-table
	@echo "All generated artifacts are up to date."

check-glob-truth-table:
	@test -f $(GLOB_TRUTH_TABLE) || { \
		echo "$(GLOB_TRUTH_TABLE) is missing; run 'make update-glob-truth-table' (requires Go)"; \
		exit 1; \
	}
	@tmp=$$(mktemp) && \
		(cd $(GLOB_GENERATE_DIR) && go mod tidy && go run .) > $$tmp && \
		diff -u $(GLOB_TRUTH_TABLE) $$tmp && rm -f $$tmp
	@echo "Truth table is up to date."
