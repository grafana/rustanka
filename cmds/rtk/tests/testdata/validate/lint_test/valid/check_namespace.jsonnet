{
  namespaceTest(manifests)::
    if std.length(manifests) > 0 then
      null
    else
      'namespace has no manifests',
}
