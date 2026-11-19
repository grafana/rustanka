local zones = ['east', 'north', 'south', 'west'];
local defaults = {
  retries: { attempts: 3, backoff: { minimum: 100, maximum: 5000 } },
  telemetry: { enabled: true, labels: { team: 'platform', tier: 'backend' } },
  limits: { requests: 1000, connections: 128 },
};

{
  application(environment, application)::
    local name = 'application-%04d' % application;
    local namespace = 'large-%03d' % environment;
    local labels = { app: name, environment: namespace };
    local routes = std.map(
      function(i) {
        key: 'route-%03d' % i,
        zone: zones[(i + environment) % std.length(zones)],
        priority: (i * 17 + application) % 37,
        path: '/%s/%s/resource/%d' % [namespace, name, i],
        upstream: '%s-%d.internal:8080' % [name, i % 8],
        headers: {
          ['x-attribute-%d' % j]: '%s-%d-%d' % [namespace, i, j]
          for j in std.range(1, 8)
        },
        weights: [((i + j + application) % 11) + 1 for j in std.range(1, 8)],
      },
      std.range(1, 32)
    );
    local ordered = std.sort(
      std.filter(function(route) route.priority % 7 != 0, routes),
      function(route) route.priority
    );
    // Persist each transformation in the manifest so lazy evaluation cannot skip the workload.
    local grouped = std.foldl(
      function(groups, route) groups + { [route.zone]: groups[route.zone] + [route] },
      ordered,
      { [zone]: [] for zone in zones }
    );
    local routing = std.mapWithKey(
      function(zone, members) {
        zone: zone,
        totalWeight: std.foldl(function(total, route) total + std.sum(route.weights), members, 0),
        byName: { [route.key]: route for route in members },
      },
      grouped
    );
    local settings = std.mergePatch(defaults, {
      telemetry: { labels: labels },
      limits: { requests: 1000 + application, connections: 128 + environment },
      routing: routing,
    });
    local configuration = std.manifestJsonEx(settings, '  ');
    local deployment = {
      apiVersion: 'apps/v1',
      kind: 'Deployment',
      metadata: { name: name, namespace: namespace, labels: labels },
      spec: {
        replicas: 2,
        selector: { matchLabels: labels },
        template: {
          metadata: { labels: labels },
          spec: {
            containers: [{
              name: name,
              image: 'example.invalid/benchmark:v1',
              ports: [{ containerPort: 8080 }],
              env: [
                { name: 'ZONE_%s' % std.asciiUpper(zone), value: '%d' % std.length(grouped[zone]) }
                for zone in zones
              ],
              volumeMounts: [{ name: 'configuration', mountPath: '/etc/application' }],
            }],
            volumes: [{ name: 'configuration', configMap: { name: name } }],
          },
        },
      },
    };
    {
      config: {
        apiVersion: 'v1',
        kind: 'ConfigMap',
        metadata: { name: name, namespace: namespace, labels: labels },
        data: { 'settings.json': configuration },
      },
      deployment: deployment {
        spec+: {
          template+: {
            metadata+: { annotations: { 'configuration-hash': std.md5(configuration) } },
          },
        },
      },
      service: {
        apiVersion: 'v1',
        kind: 'Service',
        metadata: { name: name, namespace: namespace, labels: labels },
        spec: { selector: labels, ports: [{ port: 80, targetPort: 8080 }] },
      },
    },
}
