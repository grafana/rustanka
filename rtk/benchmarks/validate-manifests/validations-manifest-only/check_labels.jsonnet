{
  manifestTest(manifest)::
    if std.objectHas(manifest.metadata, 'labels') then
      null
    else
      'manifest %s/%s is missing labels' % [manifest.kind, manifest.metadata.name],
}
