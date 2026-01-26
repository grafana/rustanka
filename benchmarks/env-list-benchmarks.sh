#!/usr/bin/env bash
set -euo pipefail

# Configuration for env list benchmarks
NUM_STATIC_ENVS=100
NUM_INLINE_FILES=9
ENVS_PER_INLINE_FILE=100
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
# RTK vs Tanka Env List Benchmarks

Comparing rtk (Rust implementation) with tk (original Tanka) for environment listing.

EOF

print_test_config
print_versions

# Create temp directory for fixtures
FIXTURES_DIR="$(mktemp -d)"
trap 'rm -rf "${FIXTURES_DIR}"' EXIT

generate_fixtures "${FIXTURES_DIR}"

# Pick one inline env directory for single-directory benchmarks
SINGLE_INLINE_DIR="${FIXTURES_DIR}/inline-01"

# Function to validate outputs match (count only, since metadata.name differs between tk/rtk for inline envs)
validate_output() {
  local name="$1"
  local tk_cmd="$2"
  local rtk_cmd="$3"
  local rtk_base_cmd="${4:-}"

  echo -n "Validating ${name}... " >&2
  tk_count=$(eval "$tk_cmd" 2>/dev/null | jq 'length' 2>/dev/null || eval "$tk_cmd" 2>/dev/null | wc -l)
  rtk_count=$(eval "$rtk_cmd" 2>/dev/null | jq 'length' 2>/dev/null || eval "$rtk_cmd" 2>/dev/null | wc -l)

  if [ "$tk_count" = "$rtk_count" ]; then
    echo -n "OK (${tk_count} envs)" >&2
  else
    echo "COUNT MISMATCH! tk=${tk_count}, rtk=${rtk_count}" >&2
    exit 1
  fi

  if [ -n "$rtk_base_cmd" ]; then
    rtk_base_count=$(eval "$rtk_base_cmd" 2>/dev/null | jq 'length' 2>/dev/null || eval "$rtk_base_cmd" 2>/dev/null | wc -l)
    if [ "$tk_count" = "$rtk_base_count" ]; then
      echo " [rtk-base OK]" >&2
    else
      echo " [rtk-base COUNT MISMATCH: ${rtk_base_count}]" >&2
      exit 1
    fi
  else
    echo "" >&2
  fi
}

echo "Validating outputs match before benchmarking..." >&2

# Build rtk-base commands if available
RTK_BASE_CMD=""
if [ -n "${RTK_BASE:-}" ]; then
  RTK_BASE_CMD="${RTK_BASE}"
fi

validate_output "single inline dir (json)" \
  "tk env list --json ${SINGLE_INLINE_DIR}" \
  "${RTK} env list --json ${SINGLE_INLINE_DIR}" \
  "${RTK_BASE_CMD:+${RTK_BASE_CMD} env list --json ${SINGLE_INLINE_DIR}}"

validate_output "single inline file (json)" \
  "tk env list --json ${SINGLE_INLINE_DIR}/main.jsonnet" \
  "${RTK} env list --json ${SINGLE_INLINE_DIR}/main.jsonnet" \
  "${RTK_BASE_CMD:+${RTK_BASE_CMD} env list --json ${SINGLE_INLINE_DIR}/main.jsonnet}"

validate_output "all envs (json)" \
  "tk env list --json ${FIXTURES_DIR}" \
  "${RTK} env list --json ${FIXTURES_DIR}" \
  "${RTK_BASE_CMD:+${RTK_BASE_CMD} env list --json ${FIXTURES_DIR}}"

validate_output "all envs (table)" \
  "tk env list ${FIXTURES_DIR}" \
  "${RTK} env list ${FIXTURES_DIR}" \
  "${RTK_BASE_CMD:+${RTK_BASE_CMD} env list ${FIXTURES_DIR}}"

echo "" >&2

# Create markdown output file (use fixed name for CI to find it)
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

# Benchmark 1: List envs from one inline env directory (--json)
echo "### Single Inline Directory (--json)" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Listing ${ENVS_PER_INLINE_FILE} environments from a single inline env directory." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

run_benchmark "${MARKDOWN_OUTPUT}.1" 3 \
  "sh -c 'tk env list --json ${SINGLE_INLINE_DIR} >/dev/null'" \
  "sh -c '${RTK} env list --json ${SINGLE_INLINE_DIR} >/dev/null'" \
  "${RTK_BASE:+sh -c '${RTK_BASE} env list --json ${SINGLE_INLINE_DIR} >/dev/null'}"
cat "${MARKDOWN_OUTPUT}.1" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Benchmark 2: List envs from one inline env file directly (--json)
echo "### Single Inline File (--json)" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Listing ${ENVS_PER_INLINE_FILE} environments from a single main.jsonnet file." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

run_benchmark "${MARKDOWN_OUTPUT}.2" 3 \
  "sh -c 'tk env list --json ${SINGLE_INLINE_DIR}/main.jsonnet >/dev/null'" \
  "sh -c '${RTK} env list --json ${SINGLE_INLINE_DIR}/main.jsonnet >/dev/null'" \
  "${RTK_BASE:+sh -c '${RTK_BASE} env list --json ${SINGLE_INLINE_DIR}/main.jsonnet >/dev/null'}"
cat "${MARKDOWN_OUTPUT}.2" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Benchmark 3: List all envs (--json)
echo "### All Environments (--json)" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Listing all $((NUM_STATIC_ENVS + NUM_INLINE_FILES * ENVS_PER_INLINE_FILE)) environments with JSON output." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

run_benchmark "${MARKDOWN_OUTPUT}.3" 1 \
  "sh -c 'tk env list --json ${FIXTURES_DIR} >/dev/null'" \
  "sh -c '${RTK} env list --json ${FIXTURES_DIR} >/dev/null'" \
  "${RTK_BASE:+sh -c '${RTK_BASE} env list --json ${FIXTURES_DIR} >/dev/null'}"
cat "${MARKDOWN_OUTPUT}.3" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Benchmark 4: List all envs (table output)
echo "### All Environments (table output)" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Listing all $((NUM_STATIC_ENVS + NUM_INLINE_FILES * ENVS_PER_INLINE_FILE)) environments with table output." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

run_benchmark "${MARKDOWN_OUTPUT}.4" 1 \
  "sh -c 'tk env list ${FIXTURES_DIR} >/dev/null'" \
  "sh -c '${RTK} env list ${FIXTURES_DIR} >/dev/null'" \
  "${RTK_BASE:+sh -c '${RTK_BASE} env list ${FIXTURES_DIR} >/dev/null'}"
cat "${MARKDOWN_OUTPUT}.4" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Output location of markdown file
echo "Markdown output written to: ${MARKDOWN_OUTPUT}" >&2
