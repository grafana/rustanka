{
  kinds: std.set(['Deployment', 'StatefulSet']),
  manifestTest(manifest)::
    if std.objectHas(manifest.spec, 'replicas') && manifest.spec.replicas > 0 then
      null
    else
      'manifest %s must have spec.replicas > 0' % [manifest.metadata.name],
}
