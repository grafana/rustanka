// Regression test for rustanka issue #30 (upstream CertainLach/jrsonnet#208):
// indexing into a list built via recursive head-concat over slices hung with
// super-linear runtime past depth ~15, because SliceArray::get evaluated every
// prefix element of the inner array instead of index-mapping in O(1).
// With the fix (upstream 7681558a and successors), depth 100 evaluates in
// milliseconds; with the bug, this test effectively hangs (>>60s).
local depth = 100;

local build(xs) = if std.length(xs) == 0 then [] else [xs[0]] + build(xs[1:]);
local r = build(std.makeArray(depth, function(j) j));

std.assertEqual(std.all([r[j] == j for j in std.range(0, depth - 1)]), true)
