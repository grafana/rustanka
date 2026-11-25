// Reproduction of a real bug found in deployment_tools where `env list`
// silently returned no environments because rtk swallowed evaluation
// errors during metadata extraction.
//
// The pattern: a multi-cluster main.jsonnet validates configs by accessing
// a `wave` field that may be missing on some entries. `tk env list` errors
// out (which is correct); rtk used to drop the error and return [].
local validateWaveNames(configs) =
  local clusters = std.objectValues(std.mapWithKey(function(k, v) { name: k, wave: v.wave }, configs));
  local invalid = std.filter(function(c) !std.member(['dev', 'ops'], c.wave), clusters);
  if std.length(invalid) > 0
  then error 'Invalid wave name for cluster(s): %s' % [std.join(',', std.map(function(c) c.name, invalid))]
  else {};

{
  envs: validateWaveNames({
    a: { wave: 'dev' },
    b: {},
  }) + {
    z: {
      apiVersion: 'tanka.dev/v1alpha1',
      kind: 'Environment',
      metadata: { name: 'z' },
      spec: { namespace: 'default' },
      data: {},
    },
  },
}
