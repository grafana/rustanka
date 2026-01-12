// Test case: Multiple environments using helmTemplate with cluster-scoped resources
// This tests recursive export with helmTemplate-generated resources including CRDs

local clusters = ['dev-us-east-0', 'prod-us-east-0'];

local makeEnv(cluster) = {
  local helmResources = std.native('helmTemplate')(
    'flagger',
    './charts/flagger-chart',
    {
      calledFrom: std.thisFile,
      namespace: 'flagger',
      values: {
        clusterName: cluster,
      },
    }
  ),

  apiVersion: 'tanka.dev/v1alpha1',
  kind: 'Environment',
  metadata: {
    name: 'flagger-' + cluster,
    labels: {
      cluster: cluster,
    },
  },
  spec: {
    apiServer: 'https://' + cluster + '.example.com:6443',
    namespace: 'flagger',
  },
  data: helmResources,
};

// Return multiple environments for recursive export
{
  ['env-' + cluster]: makeEnv(cluster)
  for cluster in clusters
}
