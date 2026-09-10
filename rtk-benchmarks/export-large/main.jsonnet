local workload = import 'workload.libsonnet';
local environmentCount = 192;
local applicationsPerEnvironment = 256;

{
  ['environment-%03d' % environment]: {
    apiVersion: 'tanka.dev/v1alpha1',
    kind: 'Environment',
    metadata: { name: 'large-%03d' % environment },
    spec: {
      apiServer: 'https://localhost:6443',
      namespace: 'large-%03d' % environment,
    },
    data: {
      ['application-%04d' % application]: workload.application(environment, application)
      for application in std.range(1, applicationsPerEnvironment)
    },
  }
  for environment in std.range(1, environmentCount)
}
