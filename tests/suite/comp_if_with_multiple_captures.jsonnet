local features = { gc: 'serialgc', libc: 'musl' };
local order = ['gc', 'libc', 'missing'];
std.assertEqual(
	[features[f] for f in order if std.objectHas(features, f)],
	['serialgc', 'musl']
)
