{
  kinds: std.set(['Deployment', 'StatefulSet']),
  manifestTest(manifest)::
    if manifest.spec.replicas > 0 then
      null
    else
      'must have > 0 replicas',
}
