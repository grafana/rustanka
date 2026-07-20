// Within-evaluation cycle garbage: creates many short-lived object cycles
// (self-referential mixins) during one evaluation. Guards the gcmodule
// auto-collection behavior (grafana leak-fix fork): without in-evaluation
// collection, cycles accumulate until the evaluation ends (~2.5x peak RSS).
// Used manually during upstream merges:
//   /usr/bin/time -v jrsonnet test_fixtures/perf/cycle-stress.jsonnet
local mk(i) =
  local o = { name: 'obj%d' % i, self_ref():: o, data: std.repeat('y', 200) };
  o { extra: super.name }.name;
std.length([mk(i) for i in std.range(0, 200000)])
