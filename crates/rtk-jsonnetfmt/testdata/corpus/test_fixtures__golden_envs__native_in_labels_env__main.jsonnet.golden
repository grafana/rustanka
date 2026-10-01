// Regression fixture: `std.native(...)` called at the environment's metadata /
// labels level (outside of `data`). This previously caused `rtk env list` to
// silently return "No environments found" because native functions were not
// registered during metadata evaluation.
{
  apiVersion: 'tanka.dev/v1alpha1',
  kind: 'Environment',
  metadata: {
    name: 'native-in-labels',
    labels: {
      // Native-function invocation in a label value — the specific pattern
      // that triggered the bug in grafana/deployment_tools#559653.
      region_match: std.toString(std.native('regexMatch')('^us-', 'us-east-1')),
      inline: 'true',
    },
  },
  spec: {
    apiServer: 'https://example.cluster:6443',
    namespace: 'default',
  },
  data: {
    configmap: {
      apiVersion: 'v1',
      kind: 'ConfigMap',
      metadata: {
        name: 'marker',
        namespace: 'default',
      },
      data: { marker: 'present' },
    },
  },
}
