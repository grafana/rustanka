local renderChart() = std.native('helmTemplate')(
  'bench',
  './charts/light-chart',
  {
    calledFrom: std.thisFile,
    namespace: 'bench',
    values: { value: 'small chart' },
  }
);

{
  ['env-%d' % i]: {
    apiVersion: 'tanka.dev/v1alpha1',
    kind: 'Environment',
    metadata: {
      name: 'bench-%d' % i,
      labels: { cluster_name: 'bench-cluster' },
    },
    spec: {
      apiServer: 'https://localhost:6443',
      namespace: 'bench-ns-%d' % i,
    },
    data: renderChart(),
  }
  for i in std.range(1, 60)
}
