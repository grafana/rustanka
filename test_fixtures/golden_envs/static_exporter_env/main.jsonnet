// Test fixture for static-exporter-style ConfigMaps with large multiline content
// This tests YAML serialization of large imported text files

local httpdConf = importstr 'httpd.conf';

{
  apiVersion: 'v1',
  kind: 'ConfigMap',
  metadata: {
    name: 'httpd-config',
    namespace: 'default',
  },
  data: {
    'httpd.conf': httpdConf,
  },
}

