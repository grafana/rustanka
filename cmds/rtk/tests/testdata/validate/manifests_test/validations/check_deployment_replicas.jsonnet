{
  kinds: std.set(['Deployment']),
  manifestTest(manifest)::
    if manifest.spec.replicas > 0 then
      null
    else
      'manifest %s must have > 0 replicas' % [manifest.metadata.name],
}
