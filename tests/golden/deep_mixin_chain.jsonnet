// Regression test for quadratic object-extension cost (rustanka#66 LayeredCores).
// Building an object from thousands of `+` mixins must not copy the full core
// list per extension: with plain Vec cores this is O(n^2) (12s at n=64000);
// with LayeredCores backpointers it is linear. n here is kept modest so the
// test also passes within cargo's smaller test-thread stacks; the wall-time
// canary for the quadratic case is still ~30x.
local n = 10000;
local objs = std.foldl(function(acc, i) acc + { ['f%d' % i]: i }, std.range(0, n - 1), {});
std.length(std.objectFields(objs))
