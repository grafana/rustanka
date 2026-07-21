// Closure-capture memory stress: many closures each using a tiny part of a
// large lexical scope. Guards the memory behavior that context capture
// analysis (upstream explicit captures, formerly rustanka UsedVars trimming)
// provides. Used manually during upstream merges:
//   /usr/bin/time -v jrsonnet test_fixtures/perf/capture-stress.jsonnet
// Compare "Maximum resident set size" against the previous release binary.
local big = { ['field%d' % i]: { value: i, pad: std.repeat('x', 100) } for i in std.range(0, 2000) };
local mk(i) =
  local a = big['field%d' % (i % 2000)];
  local unusedScope = big;  // in-scope but unused by the closure below
  function() a.value + i;
local fns = [mk(i) for i in std.range(0, 5000)];
std.foldl(function(acc, f) acc + f(), fns, 0)
