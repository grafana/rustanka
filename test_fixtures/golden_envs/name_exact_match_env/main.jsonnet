local environment(name, resourceName) = {
  apiVersion: 'tanka.dev/v1alpha1',
  kind: 'Environment',
  metadata: {
    name: name,
  },
  spec: {
    apiServer: 'https://cluster.example',
    namespace: 'default',
  },
  data: {
    config: {
      apiVersion: 'v1',
      kind: 'ConfigMap',
      metadata: {
        name: resourceName,
      },
    },
  },
};

{
  exact: environment('base', 'exact'),
  substring: environment('base-extended', 'substring'),
}
