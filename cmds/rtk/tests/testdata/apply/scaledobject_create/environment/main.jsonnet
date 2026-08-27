{
  apiVersion: 'tanka.dev/v1alpha1',
  kind: 'Environment',
  metadata: {
    name: 'test-env',
  },
  spec: {
    contextNames: ['mock-context'],
    namespace: 'default',
  },
  data: {
    scaledObject: {
      apiVersion: 'keda.sh/v1alpha1',
      kind: 'ScaledObject',
      metadata: {
        name: 'compactor-defer',
        namespace: 'default',
      },
      spec: {
        scaleTargetRef: {
          name: 'compactor',
        },
        minReplicaCount: 0,
        maxReplicaCount: 10,
      },
    },
  },
}
