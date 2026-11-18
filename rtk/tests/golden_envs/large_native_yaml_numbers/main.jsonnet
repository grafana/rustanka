local limits = std.native('parseYaml')('maximum: 9223372036854775000\nminimum: -9223372036854775000')[0];
{
  crd: {
    apiVersion: 'apiextensions.k8s.io/v1',
    kind: 'CustomResourceDefinition',
    metadata: { name: 'examples.test.example.com' },
    spec: {
      group: 'test.example.com',
      scope: 'Namespaced',
      names: { plural: 'examples', singular: 'example', kind: 'Example' },
      versions: [{
        name: 'v1', served: true, storage: true,
        schema: { openAPIV3Schema: {
          type: 'object',
          properties: { priority: limits + { type: 'integer' } },
        } },
      }],
    },
  },
}
