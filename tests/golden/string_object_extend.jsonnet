// Jsonnet spec: `e { body }` desugars to `e + { body }`, so applying an
// object literal to a non-object lhs takes `+` operator semantics — for a
// string lhs, that is string concatenation with the manifested object.
// go-jsonnet parity; regressed with upstream's strict ObjExtend lowering
// (hit in the wild by grafana dashboards defined via importstr and later
// extended with `{ ... }`).
local s = 'hello';
[
  s + { a: 1 },
  s { b: 2 },
  { c: 3 } { d: 4 },
]
