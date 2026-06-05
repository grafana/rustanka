// Benchmark fixture for rtk helmTemplate caching.
//
// Defines `count` inline environments, each rendering the same local Helm
// chart with identical parameters. Because every call uses the same release
// name, chart, namespace, and values, all calls share one cache key — mirroring
// a real fleet that renders the same chart across many clusters.
//
// Each environment calls std.native('helmTemplate') at its own call site (the
// result is intentionally not hoisted into a shared local), so with caching
// disabled helm is invoked once per environment. With rtk's in-memory cache the
// calls collapse to a single helm invocation, and with a warm --helm-cache to
// zero.

local count = 60;

local renderChart() = std.native('helmTemplate')(
  'bench',
  './charts/bench-chart',
  {
    calledFrom: std.thisFile,
    namespace: 'bench',
    values: {
      replicaCount: 2,
      image: { repository: 'nginx', tag: '1.25' },
      service: { type: 'ClusterIP', port: 8080 },
      config: { level: 'info', message: 'benchmark configmap payload' },
    },
  }
);

local makeEnv(i) = {
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
};

{
  ['env-%d' % i]: makeEnv(i)
  for i in std.range(1, count)
}
