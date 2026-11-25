local environment = std.extVar('tanka.dev/environment');

{
  apiVersion: 'v1',
  kind: 'ConfigMap',
  metadata: {
    name: if environment.spec.namespace == 'outer' then 'outer-root' else 'inner-spec-through-outer-root',
  },
  data: {
    entrypoint: 'outer',
  },
}
