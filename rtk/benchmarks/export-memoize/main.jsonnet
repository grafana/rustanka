local records = std.native('rtkMemoize')('export-memoize-records', [
  {
    metadata: {
      name: 'record-%04d' % i,
      labels: { ['label-%02d' % j]: 'value-%d-%d' % [i, j] for j in std.range(1, 12) },
    },
    spec: { owner: 'owner-%d' % (i % 32), enabled: true },
  }
  for i in std.range(1, 4096)
]);

// Fully read the shared graph once, leaving subsequent exports mostly cache hits.
local summary = std.native('rtkMemoize')('export-memoize-summary', std.length(std.manifestJson(records)));
{
  ['environment-%03d' % i]: {
    apiVersion: 'tanka.dev/v1alpha1',
    kind: 'Environment',
    metadata: { name: 'memoize-%03d' % i },
    spec: { apiServer: 'https://localhost:6443', namespace: 'memoize-%03d' % i },
    data: {
      config: {
        apiVersion: 'v1',
        kind: 'ConfigMap',
        metadata: { name: 'summary' },
        data: { bytes: std.toString(summary) },
      },
    },
  }
  for i in std.range(1, 256)
}
