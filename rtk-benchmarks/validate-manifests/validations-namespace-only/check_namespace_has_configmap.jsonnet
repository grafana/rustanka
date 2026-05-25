{
  namespaceTest(manifests)::
    local kinds = std.set([m.kind for m in manifests]);
    if std.setMember('ConfigMap', kinds) then
      null
    else
      'namespace has %d manifests but no ConfigMap' % [std.length(manifests)],
}
