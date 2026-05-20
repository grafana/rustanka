[
  {
    name: 'manifest with labels passes',
    input: {
      apiVersion: 'v1',
      kind: 'ConfigMap',
      metadata: {
        name: 'test-config',
        labels: { app: 'test' },
      },
    },
    testType: 'manifestTest',
    expectedError: null,
  },
  {
    name: 'manifest without labels fails',
    input: {
      apiVersion: 'v1',
      kind: 'ConfigMap',
      metadata: {
        name: 'test-config',
      },
    },
    testType: 'manifestTest',
    expectedError: 'manifest ConfigMap/test-config is missing labels',
  },
]
