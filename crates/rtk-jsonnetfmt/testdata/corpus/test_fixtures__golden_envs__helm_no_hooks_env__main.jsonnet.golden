// Test case: helmTemplate with noHooks option
// This tests that noHooks: true passes --no-hooks to helm, excluding hook
// resources (pre-install Jobs, etc.) from the output.
//
// Two calls are made to the same chart to exercise the cache key:
//   - without_no_hooks: noHooks omitted -- Deployment + hook Job both present
//   - with_no_hooks:    noHooks: true   -- Deployment only (hook Job excluded)
//
// If noHooks is missing from the cache key, the second call returns the first
// call's cached result, making both subtrees identical (a bug).

local withoutNoHooks = std.native('helmTemplate')(
  'hooks-enabled',
  './charts/hook-chart',
  {
    calledFrom: std.thisFile,
    namespace: 'default',
  }
);

local withNoHooks = std.native('helmTemplate')(
  'hooks-disabled',
  './charts/hook-chart',
  {
    calledFrom: std.thisFile,
    namespace: 'default',
    noHooks: true,
  }
);

{
  apiVersion: 'tanka.dev/v1alpha1',
  kind: 'Environment',
  metadata: {
    name: 'helm-no-hooks-test',
    labels: {
      cluster: 'test-cluster',
    },
  },
  spec: {
    apiServer: 'https://fwnkiegyk:6443',
    namespace: 'default',
  },
  data: {
    without_no_hooks: withoutNoHooks,
    with_no_hooks: withNoHooks,
  },
}
