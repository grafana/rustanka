{
  kinds: std.set(['Service']),
  manifestTest(manifest)::
    if std.length(manifest.spec.ports) > 0 then
      null
    else
      'service %s must have at least one port' % [manifest.metadata.name],
}
