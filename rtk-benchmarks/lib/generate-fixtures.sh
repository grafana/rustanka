#!/usr/bin/env bash
# Common fixture generation for benchmarks
# Source this file from benchmark scripts after setting configuration variables

set -euo pipefail

# Default configuration (can be overridden before sourcing)
: "${NUM_STATIC_ENVS:=100}"
: "${NUM_INLINE_FILES:=9}"
: "${ENVS_PER_INLINE_FILE:=100}"
: "${NUM_RESOURCES_PER_ENV:=20}"

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

# Generate all fixtures in FIXTURES_DIR
generate_fixtures() {
  local fixtures_dir="$1"
  
  echo "Generating test fixtures in ${fixtures_dir}..." >&2

  # Create jsonnetfile.json at root (required by tk to identify project root)
  cat >"${fixtures_dir}/jsonnetfile.json" <<'EOF'
{
  "version": 1,
  "dependencies": [],
  "legacyImports": true
}
EOF

  # Generate static environments
  echo "Generating ${NUM_STATIC_ENVS} static environments..." >&2
  for i in $(seq 1 ${NUM_STATIC_ENVS}); do
    padded=$(printf "%04d" "$i")
    env_dir="${fixtures_dir}/static-${padded}"
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
    env_dir="${fixtures_dir}/inline-${padded}"
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
}
