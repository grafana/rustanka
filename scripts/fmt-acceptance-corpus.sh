#!/bin/sh
# Requires Python 3.11+ for TOML parsing. No existing directory is deleted.
set -eu
exec "${PYTHON:-python3}" "$(dirname "$0")/fmt_acceptance_corpus.py" "$@"
