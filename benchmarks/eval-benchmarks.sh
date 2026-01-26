#!/usr/bin/env bash
set -euo pipefail

# Configuration for eval benchmarks (smaller than env-list since eval is slower)
NUM_STATIC_ENVS=1
NUM_INLINE_FILES=1
ENVS_PER_INLINE_FILE=10
NUM_RESOURCES_PER_ENV=20

# Source common fixture generation
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/lib/generate-fixtures.sh"

# Check dependencies and build
check_dependencies
build_rtk
build_rtk_base

# Extra arguments to pass to hyperfine
HYPERFINE_ARGS=("$@")

cat <<EOF
# RTK vs Tanka Eval Benchmarks

Comparing rtk (Rust implementation) with tk (original Tanka) for environment evaluation.

EOF

print_test_config
print_versions

# Create temp directory for fixtures
FIXTURES_DIR="$(mktemp -d)"
trap 'rm -rf "${FIXTURES_DIR}"' EXIT

generate_fixtures "${FIXTURES_DIR}"

# Pick one static env and one inline env file for benchmarks
SINGLE_STATIC_DIR="${FIXTURES_DIR}/static-0001"
SINGLE_INLINE_FILE="${FIXTURES_DIR}/inline-01/main.jsonnet"

# Function to validate eval outputs match
validate_eval_output() {
  local name="$1"
  local env_path="$2"

  echo -n "Validating ${name}... " >&2
  tk_output=$(tk eval "$env_path" 2>/dev/null | jq -S '.' 2>/dev/null || echo "ERROR")
  rtk_output=$(${RTK} eval "$env_path" 2>/dev/null | jq -S '.' 2>/dev/null || echo "ERROR")

  if [ "$tk_output" = "ERROR" ] || [ "$rtk_output" = "ERROR" ]; then
    echo "ERROR running eval" >&2
    exit 1
  fi

  if [ "$tk_output" = "$rtk_output" ]; then
    echo -n "OK" >&2
  else
    echo "OUTPUT MISMATCH!" >&2
    echo "tk output:" >&2
    echo "$tk_output" | head -20 >&2
    echo "rtk output:" >&2
    echo "$rtk_output" | head -20 >&2
    exit 1
  fi

  if [ -n "${RTK_BASE:-}" ]; then
    rtk_base_output=$(${RTK_BASE} eval "$env_path" 2>/dev/null | jq -S '.' 2>/dev/null || echo "ERROR")
    if [ "$rtk_base_output" = "ERROR" ]; then
      echo " [rtk-base ERROR]" >&2
      exit 1
    fi
    if [ "$tk_output" = "$rtk_base_output" ]; then
      echo " [rtk-base OK]" >&2
    else
      echo " [rtk-base OUTPUT MISMATCH]" >&2
      exit 1
    fi
  else
    echo "" >&2
  fi
}

echo "Validating eval outputs match before benchmarking..." >&2

validate_eval_output "static env" "${SINGLE_STATIC_DIR}"
validate_eval_output "inline env file" "${SINGLE_INLINE_FILE}"

echo "" >&2

# Create markdown output file
MARKDOWN_OUTPUT="${BENCHMARK_MARKDOWN_OUTPUT:-$(mktemp)}"

cat <<EOF
## Benchmarks

EOF

# Helper function to run hyperfine with optional rtk-base
run_benchmark() {
  local output_file="$1"
  local warmup="$2"
  local tk_cmd="$3"
  local rtk_cmd="$4"
  local rtk_base_cmd="${5:-}"

  local args=(-N --warmup "$warmup" "${HYPERFINE_ARGS[@]}" --export-markdown "$output_file")
  args+=(-n "tk" "$tk_cmd")
  args+=(-n "rtk" "$rtk_cmd")
  
  if [ -n "$rtk_base_cmd" ]; then
    args+=(-n "rtk-base" "$rtk_base_cmd")
  fi

  hyperfine "${args[@]}"
}

# Benchmark 1: Eval single static environment
echo "### Single Static Environment" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Evaluating a single static environment with ${NUM_RESOURCES_PER_ENV} resources (3 Kubernetes objects each)." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

run_benchmark "${MARKDOWN_OUTPUT}.1" 2 \
  "sh -c 'tk eval ${SINGLE_STATIC_DIR} >/dev/null'" \
  "sh -c '${RTK} eval ${SINGLE_STATIC_DIR} >/dev/null'" \
  "${RTK_BASE:+sh -c '${RTK_BASE} eval ${SINGLE_STATIC_DIR} >/dev/null'}"
cat "${MARKDOWN_OUTPUT}.1" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Benchmark 2: Eval inline environment file (contains multiple envs)
echo "### Inline Environment File" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Evaluating an inline environment file containing ${ENVS_PER_INLINE_FILE} environments." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

run_benchmark "${MARKDOWN_OUTPUT}.2" 2 \
  "sh -c 'tk eval ${SINGLE_INLINE_FILE} >/dev/null'" \
  "sh -c '${RTK} eval ${SINGLE_INLINE_FILE} >/dev/null'" \
  "${RTK_BASE:+sh -c '${RTK_BASE} eval ${SINGLE_INLINE_FILE} >/dev/null'}"
cat "${MARKDOWN_OUTPUT}.2" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Output location of markdown file
echo "Markdown output written to: ${MARKDOWN_OUTPUT}" >&2
