local yaml = importstr 'values.yaml';
local parsed = std.parseYaml(yaml);
local native = std.native('parseYaml')(yaml)[0];
{
  apiVersion: 'tanka.dev/v1alpha1',
  kind: 'Environment',
  metadata: { name: 'yaml-quoted-continuation' },
  spec: { apiServer: 'https://example.invalid', namespace: 'default' },
  data: {
    configmap: {
      apiVersion: 'v1',
      kind: 'ConfigMap',
      metadata: { name: 'quoted-continuation' },
      data: {
        single: parsed.properties.apiVersion.description,
        double: parsed.properties.kind.description,
        native: native.properties.apiVersion.description,
      },
    },
  },
}
