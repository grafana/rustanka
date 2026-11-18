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
      // Go declares resourceDefaults and expectVersions as plain structs, so
      // both are marshalled whatever they hold. This spec.json sets neither
      // resourceDefaults nor any of the fields inside it, and the environment
      // Jsonnet sees should still carry it as an empty object.
      wholeSpec: std.manifestJsonEx(environment.spec, '  '),
    },
  },
}
