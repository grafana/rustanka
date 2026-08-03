std.assertEqual({ assert 'a' in self : 'missing a' } + { a: 2 }, { a: 2 }) &&
test.assertThrow({ assert 'a' in self : 'missing a', b: 1 }.b, 'assert failed: missing a') &&
test.assertThrow(
  {
    _config: {
      assert $.s3_credentials_eks_use_irsa || $.s3_credentials_vault_path != '' : 's3_credentials_vault_path must be specified',
      overrides: {},
    },
    s3_credentials_eks_use_irsa: false,
    s3_credentials_vault_path: '',
    assert $._config.overrides == {},
    internal_release_version: 'v1',
  }.internal_release_version,
  'assert failed: s3_credentials_vault_path must be specified',
) &&
true
