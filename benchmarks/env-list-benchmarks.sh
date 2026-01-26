#!/usr/bin/env bash
set -euo pipefail

# Get script directory and repo root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Check dependencies
for cmd in tk hyperfine jq cargo; do
  if ! command -v "$cmd" &>/dev/null; then
    echo "Error: $cmd is required but not found in PATH" >&2
    exit 1
  fi
done

# Build rtk in release mode
echo "Building rtk in release mode..." >&2
cargo build --release -p rtk --manifest-path "${REPO_ROOT}/Cargo.toml" >&2
RTK="${REPO_ROOT}/target/release/rtk"

# Extra arguments to pass to hyperfine
HYPERFINE_ARGS=("$@")

# Configuration
NUM_STATIC_ENVS=100
NUM_INLINE_FILES=9
ENVS_PER_INLINE_FILE=100
NUM_RESOURCES_PER_ENV=20

cat <<EOF
# RTK vs Tanka Env List Benchmarks

Comparing rtk (Rust implementation) with tk (original Tanka) for environment listing.

## Test Configuration

- Static environments: ${NUM_STATIC_ENVS}
- Inline environment files: ${NUM_INLINE_FILES} (${ENVS_PER_INLINE_FILE} envs each = $((NUM_INLINE_FILES * ENVS_PER_INLINE_FILE)) total)
- Resources per environment: ${NUM_RESOURCES_PER_ENV}
- Total environments: $((NUM_STATIC_ENVS + NUM_INLINE_FILES * ENVS_PER_INLINE_FILE))

## Versions

- tk: $(tk --version)
- rtk: $(${RTK} --version)

EOF

# Create temp directory for fixtures
FIXTURES_DIR="$(mktemp -d)"
trap 'rm -rf "${FIXTURES_DIR}"' EXIT

echo "Generating test fixtures in ${FIXTURES_DIR}..." >&2

# Create jsonnetfile.json at root (required by tk to identify project root)
cat >"${FIXTURES_DIR}/jsonnetfile.json" <<'EOF'
{
  "version": 1,
  "dependencies": [],
  "legacyImports": true
}
EOF

# Function to generate resource jsonnet
generate_resources() {
  local prefix="$1"
  local count="$2"

  cat <<JSONNET
local makeConfigMap(name, idx) = {
  apiVersion: 'v1',
  kind: 'ConfigMap',
  metadata: {
    name: '%s-cm-%d' % [name, idx],
    namespace: 'ns-%s' % name,
    labels: {
      app: name,
      version: 'v%d' % idx,
      environment: '%s',
      generated: 'true',
    },
    annotations: {
      'config.hash': std.md5('%s-%d' % [name, idx]),
      'description': 'ConfigMap for %s resource %d with some longer description text that adds complexity' % [name, idx],
    },
  },
  data: {
    'config.yaml': std.manifestYamlDoc({
      server: {
        port: 8080 + idx,
        host: '0.0.0.0',
        name: '%s-server-%d' % [name, idx],
      },
      logging: {
        level: if idx % 2 == 0 then 'info' else 'debug',
        format: 'json',
        output: '/var/log/%s/%d.log' % [name, idx],
      },
      metrics: {
        enabled: true,
        port: 9090 + idx,
        path: '/metrics/%s' % name,
      },
    }),
    'settings.json': std.manifestJson({
      debug: idx % 3 == 0,
      maxConnections: 100 + idx,
      timeout: '%ds' % (30 + idx),
      features: ['feature-%d' % i for i in std.range(0, idx % 5)],
    }),
  },
};

local makeDeployment(name, idx) = {
  apiVersion: 'apps/v1',
  kind: 'Deployment',
  metadata: {
    name: '%s-deploy-%d' % [name, idx],
    namespace: 'ns-%s' % name,
    labels: {
      app: name,
      version: 'v%d' % idx,
    },
  },
  spec: {
    replicas: 1 + (idx % 5),
    selector: { matchLabels: { app: '%s-%d' % [name, idx] } },
    template: {
      metadata: { labels: { app: '%s-%d' % [name, idx] } },
      spec: {
        containers: [{
          name: '%s-container' % name,
          image: 'nginx:1.%d' % (20 + idx % 10),
          ports: [{ containerPort: 8080 + idx }],
          env: [
            { name: 'APP_NAME', value: name },
            { name: 'APP_INDEX', value: '%d' % idx },
            { name: 'COMPUTED_VALUE', value: std.md5('%s-%d' % [name, idx]) },
          ],
        }],
      },
    },
  },
};

local makeService(name, idx) = {
  apiVersion: 'v1',
  kind: 'Service',
  metadata: {
    name: '%s-svc-%d' % [name, idx],
    namespace: 'ns-%s' % name,
  },
  spec: {
    selector: { app: '%s-%d' % [name, idx] },
    ports: [{ port: 80, targetPort: 8080 + idx }],
  },
};

{
  ['cm-%d' % i]: makeConfigMap('${prefix}', i)
  for i in std.range(0, ${count} - 1)
} + {
  ['deploy-%d' % i]: makeDeployment('${prefix}', i)
  for i in std.range(0, ${count} - 1)
} + {
  ['svc-%d' % i]: makeService('${prefix}', i)
  for i in std.range(0, ${count} - 1)
}
JSONNET
}

# Generate static environments
echo "Generating ${NUM_STATIC_ENVS} static environments..." >&2
for i in $(seq 1 ${NUM_STATIC_ENVS}); do
  padded=$(printf "%04d" "$i")
  env_dir="${FIXTURES_DIR}/static-${padded}"
  mkdir -p "${env_dir}"

  # Create spec.json
  cat >"${env_dir}/spec.json" <<EOF
{
  "apiVersion": "tanka.dev/v1alpha1",
  "kind": "Environment",
  "metadata": {
    "name": "static-env-${i}",
    "labels": {
      "type": "static",
      "index": "${i}"
    }
  },
  "spec": {
    "apiServer": "https://cluster-${i}.example.com",
    "namespace": "ns-static-${i}"
  }
}
EOF

  # Create main.jsonnet with resources
  generate_resources "static-${i}" "${NUM_RESOURCES_PER_ENV}" >"${env_dir}/main.jsonnet"
done

# Generate inline environments
echo "Generating ${NUM_INLINE_FILES} inline environment files (${ENVS_PER_INLINE_FILE} envs each)..." >&2
for i in $(seq 1 ${NUM_INLINE_FILES}); do
  padded=$(printf "%02d" "$i")
  env_dir="${FIXTURES_DIR}/inline-${padded}"
  mkdir -p "${env_dir}"

  # Create main.jsonnet with multiple inline environments
  cat >"${env_dir}/main.jsonnet" <<JSONNET
local makeConfigMap(name, idx) = {
  apiVersion: 'v1',
  kind: 'ConfigMap',
  metadata: {
    name: '%s-cm-%d' % [name, idx],
    namespace: 'ns-%s' % name,
    labels: {
      app: name,
      version: 'v%d' % idx,
      environment: '%s',
      generated: 'true',
    },
    annotations: {
      'config.hash': std.md5('%s-%d' % [name, idx]),
      'description': 'ConfigMap for %s resource %d with some longer description text' % [name, idx],
    },
  },
  data: {
    'config.yaml': std.manifestYamlDoc({
      server: {
        port: 8080 + idx,
        host: '0.0.0.0',
        name: '%s-server-%d' % [name, idx],
      },
      logging: {
        level: if idx % 2 == 0 then 'info' else 'debug',
        format: 'json',
      },
    }),
    'settings.json': std.manifestJson({
      debug: idx % 3 == 0,
      maxConnections: 100 + idx,
    }),
  },
};

local makeDeployment(name, idx) = {
  apiVersion: 'apps/v1',
  kind: 'Deployment',
  metadata: {
    name: '%s-deploy-%d' % [name, idx],
    namespace: 'ns-%s' % name,
  },
  spec: {
    replicas: 1 + (idx % 5),
    selector: { matchLabels: { app: '%s-%d' % [name, idx] } },
    template: {
      metadata: { labels: { app: '%s-%d' % [name, idx] } },
      spec: {
        containers: [{
          name: '%s-container' % name,
          image: 'nginx:1.%d' % (20 + idx % 10),
          env: [
            { name: 'APP_NAME', value: name },
            { name: 'COMPUTED', value: std.md5('%s-%d' % [name, idx]) },
          ],
        }],
      },
    },
  },
};

local makeService(name, idx) = {
  apiVersion: 'v1',
  kind: 'Service',
  metadata: {
    name: '%s-svc-%d' % [name, idx],
    namespace: 'ns-%s' % name,
  },
  spec: {
    selector: { app: '%s-%d' % [name, idx] },
    ports: [{ port: 80, targetPort: 8080 + idx }],
  },
};

local makeResources(envName, envIdx) = {
  ['cm-%d' % i]: makeConfigMap(envName, i)
  for i in std.range(0, ${NUM_RESOURCES_PER_ENV} - 1)
} + {
  ['deploy-%d' % i]: makeDeployment(envName, i)
  for i in std.range(0, ${NUM_RESOURCES_PER_ENV} - 1)
} + {
  ['svc-%d' % i]: makeService(envName, i)
  for i in std.range(0, ${NUM_RESOURCES_PER_ENV} - 1)
};

{
  ['env-%03d' % j]: {
    apiVersion: 'tanka.dev/v1alpha1',
    kind: 'Environment',
    metadata: {
      name: 'inline-group-${i}-env-%03d' % j,
      labels: {
        type: 'inline',
        group: '${i}',
        index: '%03d' % j,
      },
    },
    spec: {
      apiServer: 'https://cluster-${i}-%03d.example.com' % j,
      namespace: 'ns-inline-${i}-%03d' % j,
    },
    data: makeResources('inline-${i}-env-%03d' % j, j),
  }
  for j in std.range(0, ${ENVS_PER_INLINE_FILE} - 1)
}
JSONNET
done

echo "Fixture generation complete." >&2
echo "" >&2

# Pick one inline env directory for single-directory benchmarks
SINGLE_INLINE_DIR="${FIXTURES_DIR}/inline-01"

# Function to validate outputs match (count only, since metadata.name differs between tk/rtk for inline envs)
validate_output() {
  local name="$1"
  local tk_cmd="$2"
  local rtk_cmd="$3"

  echo -n "Validating ${name}... " >&2
  tk_count=$(eval "$tk_cmd" 2>/dev/null | jq 'length' 2>/dev/null || eval "$tk_cmd" 2>/dev/null | wc -l)
  rtk_count=$(eval "$rtk_cmd" 2>/dev/null | jq 'length' 2>/dev/null || eval "$rtk_cmd" 2>/dev/null | wc -l)

  if [ "$tk_count" = "$rtk_count" ]; then
    echo "OK (${tk_count} envs)" >&2
  else
    echo "COUNT MISMATCH! tk=${tk_count}, rtk=${rtk_count}" >&2
    exit 1
  fi
}

echo "Validating outputs match before benchmarking..." >&2

validate_output "single inline dir (json)" \
  "tk env list --json ${SINGLE_INLINE_DIR}" \
  "${RTK} env list --json ${SINGLE_INLINE_DIR}"

validate_output "single inline file (json)" \
  "tk env list --json ${SINGLE_INLINE_DIR}/main.jsonnet" \
  "${RTK} env list --json ${SINGLE_INLINE_DIR}/main.jsonnet"

validate_output "all envs (json)" \
  "tk env list --json ${FIXTURES_DIR}" \
  "${RTK} env list --json ${FIXTURES_DIR}"

validate_output "all envs (table)" \
  "tk env list ${FIXTURES_DIR}" \
  "${RTK} env list ${FIXTURES_DIR}"

echo "" >&2

# Create markdown output file (use fixed name for CI to find it)
MARKDOWN_OUTPUT="${BENCHMARK_MARKDOWN_OUTPUT:-$(mktemp)}"

cat <<EOF
## Benchmarks

EOF

# Benchmark 1: List envs from one inline env directory (--json)
echo "### Single Inline Directory (--json)" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Listing ${ENVS_PER_INLINE_FILE} environments from a single inline env directory." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

hyperfine -N --warmup 3 \
  "${HYPERFINE_ARGS[@]}" \
  --export-markdown "${MARKDOWN_OUTPUT}.1" \
  -n "tk" "sh -c 'tk env list --json ${SINGLE_INLINE_DIR} >/dev/null'" \
  -n "rtk" "sh -c '${RTK} env list --json ${SINGLE_INLINE_DIR} >/dev/null'"
cat "${MARKDOWN_OUTPUT}.1" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Benchmark 2: List envs from one inline env file directly (--json)
echo "### Single Inline File (--json)" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Listing ${ENVS_PER_INLINE_FILE} environments from a single main.jsonnet file." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

hyperfine -N --warmup 3 \
  "${HYPERFINE_ARGS[@]}" \
  --export-markdown "${MARKDOWN_OUTPUT}.2" \
  -n "tk" "sh -c 'tk env list --json ${SINGLE_INLINE_DIR}/main.jsonnet >/dev/null'" \
  -n "rtk" "sh -c '${RTK} env list --json ${SINGLE_INLINE_DIR}/main.jsonnet >/dev/null'"
cat "${MARKDOWN_OUTPUT}.2" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Benchmark 3: List all envs (--json)
echo "### All Environments (--json)" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Listing all $((NUM_STATIC_ENVS + NUM_INLINE_FILES * ENVS_PER_INLINE_FILE)) environments with JSON output." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

hyperfine -N --warmup 1 \
  "${HYPERFINE_ARGS[@]}" \
  --export-markdown "${MARKDOWN_OUTPUT}.3" \
  -n "tk" "sh -c 'tk env list --json ${FIXTURES_DIR} >/dev/null'" \
  -n "rtk" "sh -c '${RTK} env list --json ${FIXTURES_DIR} >/dev/null'"
cat "${MARKDOWN_OUTPUT}.3" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Benchmark 4: List all envs (table output)
echo "### All Environments (table output)" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"
echo "Listing all $((NUM_STATIC_ENVS + NUM_INLINE_FILES * ENVS_PER_INLINE_FILE)) environments with table output." | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

hyperfine -N --warmup 1 \
  "${HYPERFINE_ARGS[@]}" \
  --export-markdown "${MARKDOWN_OUTPUT}.4" \
  -n "tk" "sh -c 'tk env list ${FIXTURES_DIR} >/dev/null'" \
  -n "rtk" "sh -c '${RTK} env list ${FIXTURES_DIR} >/dev/null'"
cat "${MARKDOWN_OUTPUT}.4" | tee -a "${MARKDOWN_OUTPUT}"
echo "" | tee -a "${MARKDOWN_OUTPUT}"

# Output location of markdown file
echo "Markdown output written to: ${MARKDOWN_OUTPUT}" >&2
