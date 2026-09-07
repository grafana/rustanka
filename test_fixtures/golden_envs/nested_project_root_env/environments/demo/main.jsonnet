// The inner jsonnetfile.json makes this directory a project root of its own.
// Expose the metadata Tanka derives so nearest-vs-outermost selection is visible.
local environment = std.extVar('tanka.dev/environment');

{
  apiVersion: 'v1',
  kind: 'ConfigMap',
  metadata: {
    name: 'project-root',
  },
  data: {
    environmentName: if std.objectHas(environment.metadata, 'name') then environment.metadata.name else '<absent>',
    environmentNamespace: if std.objectHas(environment.metadata, 'namespace') then environment.metadata.namespace else '<absent>',
  },
}
