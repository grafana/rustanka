// Tanka stores environment settings as strings and validates them only when
// the operation that uses them runs. In particular, its semver implementation
// accepts `||`, while Rust's semver parser does not. Export itself should still
// accept all of these values and expose the normalized environment to Jsonnet.
local environment = std.extVar('tanka.dev/environment');

{
  config: {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'environment-spec',
    },
    data: {
      apiServer: environment.spec.apiServer,
      diffStrategy: environment.spec.diffStrategy,
      applyStrategy: environment.spec.applyStrategy,
      expectedTanka: environment.spec.expectVersions.tanka,
      unknownVersionField: if std.objectHas(environment.spec.expectVersions, 'kubectl') then 'kept' else 'ignored',
    },
  },
}
