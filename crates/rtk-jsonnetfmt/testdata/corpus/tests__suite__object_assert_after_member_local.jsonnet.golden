local outer1 = 'one';
local outer2 = 'two';
std.assertEqual({
  local member_local = outer1,
  assert outer2 == 'two' : 'wrong outer2: ' + outer2,
  result: member_local,
}.result, 'one')
