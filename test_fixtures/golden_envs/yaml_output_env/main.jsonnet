// Test cases for YAML serialization compatibility between tk (go-yaml) and rtk (serde-saphyr)

// Prometheus alerting rules structure - tests deeply nested object-in-array indentation
// This structure specifically tests: array -> object -> field with array value -> object -> nested object
local alertingRulesData = {
  groups: [{
    name: 'prometheus-extra',
    rules: [{
      alert: 'PromScrapeFailed',
      annotations: {
        message: 'Prometheus failed to scrape a target {{ $labels.job }} / {{ $labels.instance }}',
        test: 0.00002,
        test2: '%s' % 0.00002,
        test3: '%f' % 0.00002,
      },
      expr: 'up != 1',
      'for': '15m',
      labels: {
        severity: 'warning',
      },
    }, {
      alert: 'PromScrapeFlapping',
      annotations: {
        message: 'Prometheus target flapping {{ $labels.job }} / {{ $labels.instance }}',
        runbook_url: 'https://github.com/grafana/deployment_tools/blob/master/docs/cortex/runbooks.md#BillingPromCrashlooping',
      },
      expr: 'avg_over_time(up[5m]) < 1',
      'for': '15m',
      labels: {
        severity: 'warning',
      },
    }],
  }],
};

// Nested YAML data for literal block test - tests indentation of nested mappings
local nestedYamlData = {
  enabled: true,
  rules: [
    {
      labels: {
        clientId: '$3',
        partition: '$5',
        topic: '$4',
      },
      nested_again: [
        {
          hello: 'world',
        },
        {
          hello: 'world',
        },
      ],
      name: 'test_rule_1',
      // String with angle brackets and colon - tk quotes, rtk doesn't
      pattern: 'server<type=metrics, listener=(.+), processor=(.+)><>(.+):',
    },
  ],
};

local configData = {
  database: {
    host: 'localhost',
    port: 5432,
    name: 'myapp',
  },
  features: {
    enableCache: true,
    float: 8.1,
    // Test large float formatting: tk uses scientific notation for large numbers,
    // rtk may not. 3333333.333333333 vs 3.333333333333333e+06
    max_series: 10000000 / 3,
  },
  servers: [
    {
      name: 'server1',
      ip: '10.0.0.1',
      dependencies: [
        {
          name: 'dependency1',
          version: '1.0.0',
          dependencies: [
            {
              name: 'dependency2',
              version: '2.0.0',
              description: |||
                This is a dependency description.
                This is a second line.
              |||,
            },
          ],
        },
        {
          name: 'dependency3',
          version: '3.0.0',
          description: |||
            This is a dependency description.
            This is a second line.
          |||,
          dependencies: [
            {
              name: 'dependency4',
              version: '4.0.0',
              description: |||
                This is a dependency description.
                This is a second line.
              |||,
            },
          ],
        },
      ],
    },
    {
      name: 'server2',
      ip: '10.0.0.2',
    },
  ],
};

// Test for @-prefixed keys quote style: tk uses single quotes '@type':, rtk uses double quotes "@type":
// This is common in Envoy configuration
local envoyConfig = {
  static_resources: {
    listeners: [{
      filter_chains: [{
        filters: [{
          typed_config: {
            '@type': 'type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager',
            http_filters: [{
              typed_config: {
                '@type': 'type.googleapis.com/envoy.extensions.filters.http.router.v3.Router',
              },
            }],
          },
        }],
      }],
    }],
  },
};

// Multi-line HTML content for testing string style (inline quoted with \n vs literal block)
local htmlContent = |||
  <html>
    <head>
      <style>
        body { font-family: sans-serif; }
        .container { margin: 1rem; }
      </style>
    </head>
    <body>
      <h1>Test Page</h1>
      <p class="description">This is a test page with HTML content.</p>
    </body>
  </html>
|||;

{
  configmap: {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'app-config',
      namespace: 'default',
    },
    data: {
      'config.yaml': std.manifestYamlDoc(configData),
      'config.json': std.manifestJson(configData),
      'simple.txt': 'Hello, World!',
      'multilineMangled.txt': std.manifestYamlDoc({
        multilineMangled: |||
          This is a multiline string.
          This is a second line. It has an intentional trailing space. tk mangles it. 
        |||,
        otherField: 'otherValue',
      }),
    },
  },
  // Test deeply nested object-in-array indentation (Prometheus alerting rules structure)
  'alerting-rules-configmap': {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'alerting-rules',
      namespace: 'default',
    },
    data: {
      'alerts.rules': std.manifestYamlDoc(alertingRulesData),
      'alerts.rules.manifestYamlFromJson': std.native('manifestYamlFromJson')(std.toString(alertingRulesData)),
      '12': 'test',
      '12.5': 'test2',
      test: '00:00',
      description: "Automatic connection preference. Set to true for 'ACCEPT_AUTOMATIC' or false for 'ACCEPT_MANUAL'",
      test2: 'externalIPs is a list of IP addresses for which nodes in the cluster will also accept traffic for this service.  These IPs are not managed by Kubernetes.  The user is responsible for ensuring that traffic arrives at a node with this IP.  A common example is external load-balancers that are not part of the Kubernetes system.',
      expr: 'max by (namespace, provider, collector) (cloudcost_exporter_collector_last_scrape_duration_seconds) > 50',
      bytes_threshold: 7500000,
      message: "There are running tests that don't have any metric rows inserted for 10 minutes",
      message2: 'No new in-progress runners for over 20m while jobs remain queued. This might indicate rate limiting, scheduling issues, etc.',
    },
  },
  // Test with quote_keys=false (matches real-world usage where keys are unquoted)
  'alerting-rules-unquoted-configmap': {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'alerting-rules-unquoted',
      namespace: 'default',
    },
    data: {
      // indent_array_in_object=false, quote_keys=false
      'alerts.rules': std.manifestYamlDoc(alertingRulesData, false, false),
    },
  },
  // ConfigMap with dashboard JSON containing floats
  'configmap-dashboard': {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'dashboards',
      namespace: 'default',
    },
    data: {
      // Dashboard JSON as string - matching real Grafana dashboard format
      'dashboard.json': import 'dashboard-promtail.json',
      'dashboard-to-string.json': std.toString(import 'dashboard-promtail.json'),
      'dashboard2-to-string.json': std.toString(import 'dashboard-cle-headquarters.json'),
      'dashboard3-to-string.json': std.toString(import 'ds-querier-cluster-deployment.libsonnet'),
    },
  },
  // Test nested YAML indentation in literal block strings
  'nested-yaml-configmap': {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'nested-yaml-config',
      namespace: 'default',
    },
    data: {
      [if false then 'RATE_LIMIT_ORG_OVERRIDES' else null]: 'test',
      [if true then 'RATE_LIMIT_ORG_OVERRIDES2' else null]: 'test',
      pruneTest: std.prune({
        nested: {
          test: null,
        },
        test: null,
      }),
      test_quoting: ':9121',
      description: 'Cell of type alert-manager on cluster prod-us-east-3 and namespace alertmanager',
      'rules.yml': std.manifestYamlDoc(nestedYamlData, quote_keys=false),
      'rules.yml.quoted': std.manifestYamlDoc(nestedYamlData, quote_keys=true),
      'rules.yml.quoted.indented': std.manifestYamlDoc(nestedYamlData, quote_keys=true, indent_array_in_object=true),
    },
  },
  // Test nested YAML indentation in literal block strings
  'nested-yaml-manifest-yaml-from-json-configmap': {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'nested-yaml-manifest-yaml-from-json-config',
      namespace: 'default',
    },
    data: {
      'rules.yml': std.native('manifestYamlFromJson')(std.manifestJson(nestedYamlData)),
    },
  },
  // Test case for asterisk quoting: tk uses single quotes ('*'), rtk uses double quotes ("*")
  'wildcard-resource': {
    apiVersion: 'example.io/v1',
    kind: 'WildcardConfig',
    metadata: {
      name: 'wildcard-test',
      namespace: 'default',
    },
    spec: {
      // Asterisk wildcard - tk quotes with single quotes, rtk with double quotes
      allowedPrincipal: '*',
      patterns: ['*', 'prefix-*', '*-suffix'],
    },
  },
  // Test multi-line HTML string representation (literal block vs inline with \n)
  'html-configmap': {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'html-content',
      namespace: 'default',
    },
    data: {
      'index.html': htmlContent,
    },
  },
  // Test @-prefixed keys quote style: tk uses single quotes, rtk uses double quotes
  'envoy-configmap': {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'envoy-config',
      namespace: 'default',
    },
    data: {
      'envoy.yaml': std.native('manifestYamlFromJson')(std.manifestJson(envoyConfig)),
    },
  },
  // Test long string line wrapping in array context
  // tk wraps long strings like "--reason=Removing Flux ignores before scheduled rollout of this cell"
  // to multiple lines, rtk may not wrap the same way
  // Test large float scientific notation: tk uses 3.333333333333333e+06, rtk uses 3333333.333333333
  // This tests the outer YAML serializer (Tanka's manifestYamlFromJson path)
  'overrides-configmap': {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'overrides',
      namespace: 'default',
    },
    data: {
      // Uses manifestYamlFromJson (Tanka's YAML path with scientific notation threshold)
      'overrides.yaml': std.native('manifestYamlFromJson')(std.manifestJson({
        tenant_limits: {
          max_series: 10000000 / 3,  // ~3.33 million - above threshold
          max_samples: 1500000,  // 1.5 million - above threshold
          small_value: 999999,  // below 1 million threshold
        },
      })),
    },
  },
  cronjob: {
    apiVersion: 'batch/v1',
    kind: 'CronJob',
    metadata: {
      name: 'remove-flux-ignores-before-rollout',
      namespace: 'default',
    },
    spec: {
      schedule: '0 9 * * 1-5',
      jobTemplate: {
        spec: {
          template: {
            spec: {
              containers: [{
                name: 'kubectl',
                image: 'bitnami/kubectl:1.25',
                args: [
                  // This long string triggers different wrapping behavior between tk and rtk
                  '--reason=Removing Flux ignores before scheduled rollout of this cell',
                  '--namespace=mimir-ops-03',
                  '--selector=app.kubernetes.io/name=mimir',
                ],
              }],
            },
          },
        },
      },
    },
  },
  // Test long string wrapping and continuation line indentation
  scaledobject: {
    apiVersion: 'keda.sh/v1alpha1',
    kind: 'ScaledObject',
    metadata: {
      name: 'test-scaled',
      namespace: 'default',
    },
    spec: {
      triggers: [{
        type: 'prometheus',
        metadata: {
          // Long query string that triggers line wrapping - tests continuation indentation
          // tk uses 8-space indent for continuation, rtk uses 4-space
          query: '(1 - (min(kubelet_volume_stats_available_bytes{cluster="test-cluster", namespace="test-ns", persistentvolumeclaim=~"store-gateway-.*"}/kubelet_volume_stats_capacity_bytes{cluster="test-cluster",namespace="test-ns", persistentvolumeclaim=~"store-gateway-.*"}))) * 100',
        },
      }],
    },
  },
  // Test CSP header with embedded single quotes - tk preserves quotes, rtk may not
  'csp-deployment': {
    apiVersion: 'apps/v1',
    kind: 'Deployment',
    metadata: {
      name: 'csp-test',
      namespace: 'default',
    },
    spec: {
      template: {
        spec: {
          containers: [{
            name: 'nginx',
            env: [{
              name: 'CSP_HEADER',
              // CSP value with embedded single quotes - tests quote preservation and line wrapping
              value: "frame-ancestors https://labclient.labondemand.com https://university.grafana.com; script-src 'self' 'strict-dynamic' 'unsafe-eval' 'report-sample' *.googleadservices.com *.marketo.net *.facebook.net *.google-analytics.com https://*.clearbitjs.com https://app.clearbit.com https://snap.licdn.com https://www.google-analytics.com/analytics.js https://px.ads.linkedin.com https://www.linkedin.com https://fresnel.vimeocdn.com https://f.vimeocdn.com https://vimeo.com https://player.vimeo.com https://platform.twitter.com object-src 'none'; upgrade-insecure-requests; report-to csp-endpoint; report-uri /api/csp-reports; ",
            }],
          }],
        },
      },
    },
  },
  // Test case for long string wrapping behavior (tk wraps at ~80 chars, rtk doesn't)
  deployment: {
    apiVersion: 'apps/v1',
    kind: 'Deployment',
    metadata: {
      name: 'test-deployment',
      namespace: 'default',
    },
    spec: {
      template: {
        spec: {
          containers: [{
            name: 'test-container',
            // Test empty arrays - tk uses [], rtk uses block style
            env: [],
            volumeMounts: [],
            args: [
              // Long string that should trigger line wrapping
              '-hosted_grafana_api_config=[{"address": "http://api-headless.hosted-grafana.svc.cluster.local.","slug": "prod-sa-east-0","token": "$(HG_CLUSTER_TOKEN)"}]',
              // Another long string
              'kubectl config set-cluster test-cluster --embed-certs=true --certificate-authority=/var/run/certs/kubernetes/ca.pem.crt --server=https://apiserver.test.svc.cluster.local.:443 --kubeconfig=/var/run/secrets/kubernetes/kubeconfig',
              // String starting with dash containing special chars - tk unquoted, rtk quotes
              '-forward.selectors={__name__="target_info"},{__name__="traces_info"}',
            ],
          }],
          initContainers: [{
            name: 'secrets-copier',
            image: 'busybox:1.34',
            command: ['/bin/sh', '-exc'],
            args: [
              // Long shell command that triggers different line wrapping between tk and rtk
              'chown -R nobody:nobody /var/run/secrets/database;cp /var/run/certs/database/ca.crt /var/run/secrets/database/ca.crt;chown nobody:nobody /var/run/secrets/database/ca.crt;chmod 600 /var/run/secrets/database/ca.crt;cp /var/run/certs/database/client.root.crt /var/run/secrets/database/client.root.crt;chown nobody:nobody /var/run/secrets/database/client.root.crt;chmod 600 /var/run/secrets/database/client.root.crt;cp /var/run/certs/database/client.root.key /var/run/secrets/database/client.root.key;chown nobody:nobody /var/run/secrets/database/client.root.key;chmod 600 /var/run/secrets/database/client.root.key',
            ],
          }],
          // Test empty arrays at pod level
          volumes: [],
        },
      },
    },
  },
}
