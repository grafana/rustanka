[
  {
    name: 'non-empty namespace passes',
    input: [
      {
        apiVersion: 'v1',
        kind: 'ConfigMap',
        metadata: { name: 'test' },
      },
    ],
    testType: 'namespaceTest',
    expectedError: null,
  },
  {
    name: 'empty namespace fails',
    input: [],
    testType: 'namespaceTest',
    expectedError: 'namespace has no manifests',
  },
]
