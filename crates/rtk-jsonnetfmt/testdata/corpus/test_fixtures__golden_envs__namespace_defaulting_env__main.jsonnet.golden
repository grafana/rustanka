// Go's `namespace` carries no `omitempty`, and `spec.Parse` starts from
// `v1alpha1.New()`, which sets it to `default` before unmarshalling. So a
// `spec.json` that says nothing about the namespace still reports one, both to
// the environment reading its own spec and to whatever consumes the export.
//
// Every other static fixture names a namespace explicitly, so nothing else
// covers the defaulting.
local environment = std.extVar('tanka.dev/environment');

{
  probe: {
    apiVersion: 'v1',
    kind: 'ConfigMap',
    metadata: {
      name: 'namespace-defaulting',
    },
    data: {
      // Read directly, which fails outright where the field is missing rather
      // than merely absent from the manifested spec.
      namespace: environment.spec.namespace,
      wholeSpec: std.manifestJsonEx(environment.spec, '  '),
    },
  },
}
