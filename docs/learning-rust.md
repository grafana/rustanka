# Learning Rust through rustanka

A working doc. Jamal is learning Rust by reading this codebase rather than a
tutorial, so every concept here is tied to real code in this repository that
was written for a real reason.

## How to use this doc

**If you are Jamal:** read top to bottom the first time. After that, jump to
whatever section matches the code you are looking at. The quiz at the end is
the checkpoint — try it cold, then look up what you missed.

**If you are an agent session:** you are expected to add to this. The rules:

1. **Append, don't rewrite.** Earlier sections are things he has already read.
   Rewriting them silently loses the reading he has already done.
2. **Every concept gets three things**: what it is in general Rust terms, a
   real place in this repo where it appears, and *why it is there* — the
   problem it was solving. The third is the part tutorials leave out and the
   part that makes it stick.
3. **Add to the quiz when you add a concept**, with the answer in the Answers
   section. Keep questions answerable from this doc alone.
4. **Keep it honest.** If something in Rust is genuinely awkward, say so. He
   will hit the awkwardness anyway and it is better to have been warned.
5. **Update the log at the bottom** so the next session knows what changed.

---

## The 30-second map

`rtk` is a Rust rewrite of `tk` (Tanka), a Go tool. The hard requirement is
that `rtk` produces **byte-identical output** to `tk`, which is why so much of
this codebase is a deliberate line-by-line port of Go rather than a fresh
design.

The repository is a Cargo **workspace**: many small libraries (`crates/`) plus
a few binaries (`cmds/`). Two names to know:

- **crate** — one compiled unit. Roughly "a library" or "a binary". Each
  directory under `crates/` is one.
- **module** — a namespace *inside* a crate, usually one file. `src/pass.rs`
  is the `pass` module of the `rtk-jsonnetfmt` crate.

`crates/rtk-jsonnetfmt` is the one this doc mostly draws on: it reimplements
`tk fmt`, the code formatter.

---

## 1. How the formatter works, because the Rust makes more sense with it

Three steps:

1. **Lex and parse** — turn text into a tree (an *AST*, abstract syntax tree).
2. **Passes** — twelve small independent tidy-ups applied to the tree.
3. **Unparse** — turn the tree back into text.

The odd word you will see everywhere is **fodder**: the comments and blank
lines. A normal compiler throws those away. A formatter cannot, so they are
carried along attached to the tree. Most of the difficulty in this crate is
fodder bookkeeping.

Three of the twelve passes exist so far:

| Pass | What it does |
| --- | --- |
| `FixTrailingCommas` | `[1, 2,]` on one line → drop the comma. Spread over lines → add one. |
| `NoRedundantSliceColon` | `a[1:2::]` → `a[1:2:]` |
| `PrettyFieldNames` | `{ ['foo']: 1 }` → `{ foo: 1 }`, and `a['foo']` → `a.foo` |

---

## 2. Structs and enums: how the tree is shaped

**A struct** is a record with named fields — like a Go struct or a Python
dataclass. `crates/rtk-jsonnetfmt/src/ast.rs`:

```rust
pub struct Array {
    pub elements: Vec<CommaSeparatedExpr>,
    pub close_fodder: Fodder,
    pub trailing_comma: bool,
}
```

**An enum** is where Rust differs from Go in a way that matters. A Rust enum is
not a list of constants — each variant can *carry different data*:

```rust
pub enum NodeKind {
    Array(Array),              // an array node carries an Array
    LiteralBoolean(bool),      // a boolean carries just a bool
    LiteralNull,               // null carries nothing
    Object(Object),
    // ...24 more
}
```

This is the single most useful thing in Rust. It is how you say "a node is
exactly one of these 28 things, and here is the data for each". Go has no
equivalent and fakes it with interfaces plus type switches — which is exactly
what upstream go-jsonnet does, and why the port reads differently.

`match` then forces you to handle every variant. Add a 29th node kind and
every `match` in the crate stops compiling until you deal with it. That is a
feature: it is how a refactor becomes safe.

```rust
match &node.kind {
    NodeKind::Array(array) => { /* `array` is the Array struct */ }
    NodeKind::LiteralNull => { /* nothing to unpack */ }
    // ... if you miss one, it does not compile
}
```

---

## 3. `Option<T>`: no null

Rust has no null. "Maybe a thing" is `Option<T>`, an enum with two variants:
`Some(value)` or `None`.

```rust
pub struct Index {
    pub id: Option<Identifier>,     // either `a.foo`
    pub index: Option<Box<Node>>,   // or `a['foo']` — exactly one is set
}
```

You cannot use the value without unwrapping it first, and the usual way is
`if let`:

```rust
if let Some(index) = &mut node.index {
    pass.visit(index, ctx);     // only runs when there is one
}
```

**Why this matters here, concretely.** `internal/pass/pass.go` upstream does
`p.Visit(p, &field.Expr1, ctx)` for a field kind where `Expr1` is guaranteed
non-nil by the parser. If that guarantee were ever wrong, Go would panic on a
nil dereference. The Rust port's `if let` just does nothing. Same behaviour for
every real input, no crash for the unreal one. Note that this is not Rust being
clever — it is the compiler refusing to let you write the crashing version.

(`Box<Node>` above means "a `Node` stored on the heap". A tree node containing
other nodes needs it, or the type would be infinitely large. Think of it as a
pointer that owns what it points to.)

---

## 4. Ownership and borrowing — the big one

This is the concept Rust is famous for and the one that shaped our code most.

Rust has no garbage collector, and it does not make you free memory by hand
either. Instead, every value has exactly one **owner**, and when the owner goes
out of scope the value is freed. You can lend out **references** instead of
transferring ownership, and there are exactly two kinds:

- `&T` — a shared reference. You may have **many** at once. Read-only.
- `&mut T` — an exclusive reference. You may have **exactly one**, and no
  `&T` at the same time. Read and write.

That one rule prevents an enormous class of bugs — two parts of a program
mutating the same thing and disagreeing about it — and it is checked entirely
at compile time, so it costs nothing at runtime.

**Where it bit us.** These passes move a comment from one field of a struct to
another. That needs two `&mut` at once — one to read from, one to write to:

```rust
// This does NOT compile: two &mut borrows of `field` alive together.
field.name.fodder.move_front(&mut field.fodder1);
```

The fix is **destructuring**: name the fields separately, so the compiler can
see they are different pieces of memory and not overlapping.

```rust
let ObjectField { fodder1, fodder2, op_fodder, expr1, .. } = &mut *field;
// now `fodder1` and `op_fodder` are independent &mut, usable together
op_fodder.move_front(fodder2);
```

`..` means "and ignore the rest of the fields". You will see this pattern all
over `src/passes/` and now you know why it is there rather than the obvious
`field.x` spelling.

**Two words you will hear.** *Moving* a value gives ownership away — the old
variable is then unusable. *Cloning* makes a copy so both can live.
`src/passes/pretty_field_names.rs` clones a small bit of fodder for exactly
this reason: Go reads it from an object after unlinking that object, which its
garbage collector makes safe; Rust needs the copy taken first.

---

## 5. Traits: interfaces that can carry behaviour

A **trait** is like a Go interface or a Java interface — a set of methods a
type promises to have. The extra power is that a trait method can have a
**default body**, so implementors inherit behaviour rather than just a
signature.

`src/pass.rs` defines `AstPass` with about thirty methods, each defaulting to
"walk one level deeper into the tree". A pass then overrides only what it
changes:

```rust
impl AstPass for NoRedundantSliceColon {
    type Ctx = ();

    fn base_context(&mut self) {}

    fn slice(&mut self, node: &mut Slice, ctx: &()) {
        // ...the one thing this pass does...
        base::slice(self, node, ctx);   // then carry on walking
    }
}
```

One override out of thirty. `PrettyFieldNames` overrides two.

**`()` is the empty type**, pronounced "unit". `type Ctx = ()` says "this pass
carries no extra context". It is Rust's equivalent of `void`, except it is a
real value you can pass around.

**The awkward bit, told honestly.** In Java or Python a subclass calls
`super.foo()` to run the parent's version. **Rust has no `super`.** So the
"walk deeper" logic lives in plain functions in a `base` module, and both the
default method *and* an overriding pass call the same function — that is the
`base::slice(self, node, ctx)` line above. It is the one place the port could
not follow Go's structure, and it is documented at the top of `src/pass.rs` so
nobody "tidies" it later.

---

## 6. `Result<T, E>`: errors are values

Rust has no exceptions. A function that can fail returns `Result<T, E>` —
another enum, either `Ok(value)` or `Err(error)`.

```rust
pub fn format(filename: &str, input: &str, options: &Options)
    -> Result<String, Error>
```

The `?` operator is the everyday tool. This line:

```rust
let (mut node, mut final_fodder) = parser::snippet_to_raw_ast(filename, input)?;
```

means "if that returned `Err`, return it from `format` immediately; otherwise
unwrap the `Ok` and carry on". It is the ergonomics that make error-as-value
bearable, and you will see `?` on almost every fallible call in the codebase.

---

## 7. `String` vs `&str`, `Vec<T>` vs `&[T]`

The pairing that confuses everyone first. In both cases: the first owns its
data, the second is a borrowed view of someone else's.

- `String` — an owned, growable string. `&str` — a borrowed slice of one.
- `Vec<T>` — an owned, growable list. `&[T]` — a borrowed window into one.

Rule of thumb: **take `&str` and `&[T]` as function parameters, return
`String` and `Vec<T>`.** That way callers can pass what they already have
without copying, and you hand back something they own. `format(filename: &str,
input: &str) -> Result<String, Error>` follows it exactly.

---

## 8. Tests live next to the code

Two flavours in this repo:

- **Unit tests** in the same file as the code, in a `mod tests` block marked
  `#[cfg(test)]` (meaning "only compile this when testing"). See the bottom of
  `src/passes/fix_trailing_commas.rs` — they can reach private internals.
- **Integration tests** in `tests/`, each file compiled as its own separate
  program that can only use the crate's public API. `tests/pass_parity.rs` is
  one.

`cargo test` runs both. A useful flag: `--no-fail-fast`, because by default
cargo stops at the first failing test *file* and quietly skips the rest.

---

## 9. The testing idea worth stealing

Not Rust-specific, and the most transferable thing here.

The job is "match Go exactly". The tempting way to do that is to read the Go
source carefully and write the Rust to match. **That was measured on this
project and it does not work well enough**: of 16 expectations derived by
reading Go's source, 2 were wrong. A careful reading of a different Go library
shipped nine bugs.

So instead: small Go programs run the *real* implementation and dump its
answers to JSON, committed to the repo, and the Rust tests compare against
those files. Three of them now — one for the lexer, one for the parser, one for
each formatter pass — regenerated by `make update-fmt-*-oracle`.

It earned its keep immediately. The newest one caught a wrong claim on its
first run: I had concluded from a text diff that `FixTrailingCommas` changed
none of the 138 test files. It changes one — the effect was just invisible
because that file also needs passes that do not exist yet. **A pass changing a
file is not the same as that file's output changing**, and only the oracle could
see the difference.

The general lesson: when you must match a reference implementation, make it
tell you the answers. Do not infer them.

---

## 10. Slice patterns: matching on the *shape* of a list

In Go you check a length and then index:

```go
if len(element.Comment) == 1 {
    comment := &element.Comment[0]
    // ... use comment
}
```

Two steps, and the second one can be wrong independently of the first — nothing
stops you writing `[1]` there. Rust lets you say both at once:

```rust
if let [comment] = element.comment.as_mut_slice() {
    // `comment` is a `&mut String`, and there is exactly one
}
```

`[comment]` is a **slice pattern**. It matches only a slice of length one, and
in doing so it *names* the single element. There is no index to get wrong and
no length check to forget, because they are the same expression.

The family is worth knowing:

| Pattern | Matches |
| --- | --- |
| `[]` | empty |
| `[x]` | exactly one, bound to `x` |
| `[first, second]` | exactly two |
| `[first, ..]` | one or more; `first` is the head |
| `[.., last]` | one or more; `last` is the tail |
| `[first, .., last]` | two or more |
| `[first, rest @ ..]` | head plus `rest`, itself a slice |

**Where it is:** `crates/rtk-jsonnetfmt/src/passes/enforce_comment_style.rs`.

**Why it is there:** the pass rewrites `# c` into `// c`, but only for
*single-line* comments. A multi-line `/* … */` is stored as one string per
line, so `len == 1` is exactly the test "this is a one-line comment". The slice
pattern makes the code say that, rather than saying "check a number, then index
by hand and hope".

Note `as_mut_slice()`. A `Vec<String>` is not itself a slice; that call hands
out a `&mut [String]` view of it, which is what a slice pattern matches
against. On a read-only path you would write `as_slice()`, or just pass
`&the_vec`.

---

## 11. Strings are not byte arrays, and `&s[2..]` can panic

This is the sharpest Rust-versus-Go difference in the 2c work, and it has a
correctness edge to it, not just a style one.

Go's strings are byte slices with no guarantees. `s[0] == '/'` reads a byte;
`s[2:]` takes everything from byte two. Both always compile and always run.
Upstream does exactly that:

```go
if (*comment)[0] == '/' {
    *comment = "#" + (*comment)[2:]
}
```

Rust's `String` and `&str` are **guaranteed to be valid UTF-8**, and the type
system defends that guarantee. `&s[2..]` compiles, but if byte 2 lands in the
middle of a multi-byte character it **panics at runtime** — the slice it would
return is not valid UTF-8, so Rust refuses to make one rather than hand you a
broken string. `s[0]` does not even compile: indexing a `str` by a single
number is not allowed, precisely because "the byte at 0" and "the character at
0" are different questions and Rust will not let you conflate them silently.

So the port says what it means:

```rust
if comment.starts_with('/') {
    *comment = format!("#{}", &comment[2..]);
}
```

- `starts_with('/')` replaces the byte read. It is total: no panic on an empty
  string, no index to get wrong. (Go's version *would* panic on an empty
  comment; the lexer cannot produce one, so the behaviour is the same for every
  input that exists — but the Rust version cannot panic at all.)
- `&comment[2..]` is kept, and it is safe here for the same reason it is in Go:
  a one-line comment starting with `/` starts with `//`, two ASCII bytes.

Three tools for when a byte index is not obviously on a boundary:

- `s.as_bytes().get(1)` — returns `Option<&u8>`, so a short string gives `None`
  instead of panicking. The port uses this for "is byte 1 a `!`?".
- `s.chars()` — iterate characters rather than bytes.
- `s.strip_prefix("//")` — returns `Option<&str>` and does the check and the
  removal together. Often the nicest answer; the port keeps the `[2..]` form
  only because it mirrors the Go line it is porting.

**Why the awkwardness is worth it:** this crate's other half,
`src/string_util.rs`, is a port of Go code that *does* index bytes into
arbitrary user strings — and its module comment records that slicing the `&str`
instead would panic on input as ordinary as `'\u12€x'`. Rust made that a
visible decision rather than a latent bug.

---

## 12. What `derive` gives you, and why `Copy` was left off on purpose

`#[derive(...)]` writes a trait implementation for you:

```rust
#[derive(Debug, Clone, Copy)]
pub struct EnforceStringStyle {
    style: StringStyle,
}
```

- **`Debug`** — printable with `{:?}`. Needed by `assert_eq!` failure messages,
  so effectively mandatory.
- **`Clone`** — an explicit `.clone()` makes a copy.
- **`Copy`** — the value is copied *implicitly*, on every assignment and every
  pass to a function, and the original stays usable. Only for small
  plain-data types.
- **`Default`** — a zero value. 2c added this to `LocationRange`, deliberately
  documented as "Go's zero value", for one caller that needs a location it will
  never look at.

The interesting one is the `Copy` that is **not** there:

```rust
#[derive(Debug, Clone)]   // no Copy
pub struct EnforceCommentStyle {
    style: CommentStyle,
    seen_first_fodder: bool,
}
```

It would compile. An enum and a bool are both `Copy`, so the derive would be
accepted. It is left off because this struct has **state**: `seen_first_fodder`
starts false and flips once the traversal has seen the first comment, and that
is what makes a `#!/usr/bin/env jsonnet` line at the top of a file survive
formatting.

With `Copy`, writing `let mut p = pass;` would silently make a second pass with
its own copy of the flag — the compiler would say nothing, the tests would
mostly still pass, and the hashbang rule would quietly re-arm halfway through a
file. Without `Copy`, that line *moves* the pass, and any later use of the
original is a compile error.

**The general habit:** `Copy` is the right default for a small value with no
history — a `StringStyle`, a `Location`, a count. It is the wrong default for
anything whose current value depends on what has happened so far. Leaving a
derive off is a design decision, and worth a comment saying so.

---

## 13. Disjoint field borrows: the rule is about *places*, not values

§4 said you may have only one `&mut` to a value at a time. That is true, and it
would make `FixIndentation` impossible to write if it meant what it sounds like.
This compiles:

```rust
self.fill(&mut field.op_fodder, true, true, indent);
if let Some(expr3) = field.expr3.as_deref_mut() {
    self.visit(expr3, indent, true);
}
```

Two `&mut` borrows out of the same `field`. The compiler allows it because it
tracks **places** — `field.op_fodder` and `field.expr3` are different paths, so
borrowing one says nothing about the other. This is why §4's destructuring trick
was needed: there, both borrows went through `field` *itself*, which is one
place.

The practical shape: reach for a field directly and you are usually fine; reach
for the whole struct twice and you need to destructure.

**Where it is in the repo:** all through
`crates/rtk-jsonnetfmt/src/fix_indentation.rs` — the `ObjectFieldKind::Assert`
arm of `fields` is the clearest, borrowing `fodder1`, `op_fodder`, `expr2` and
`expr3` in turn.

**Why it is there:** the Go it ports takes a *copy* of each field struct and
mutates through the slice headers inside it, which happens to work because a Go
slice copy shares its backing array. Rust has no equivalent sleight of hand, so
the port has to borrow each slot explicitly — and the borrow checker turns out
to allow exactly the accesses Go was making by accident.

---

## 14. `&mut self` and `&mut arg` do not conflict

The other thing that makes the above practical, and it trips people up because
it looks like two mutable borrows:

```rust
fn fill(&mut self, fodder: &mut Fodder, /* … */) { /* … */ }

// called as:
self.fill(&mut node.close_fodder, false, false, indent);
```

`self` is the `FixIndentation` — it owns a column counter. `node` is part of the
syntax tree. They are unrelated values, so `&mut self` and `&mut node.…` are
simply two borrows of two different things. No conflict, no cloning, no
`RefCell`.

It is worth stating because a pass *feels* like it is "inside" the tree it walks.
It is not. It is a separate object holding a cursor.

**Why it matters here:** `fill` both writes indentation into the fodder and
advances `self.column`. If those two had lived in one object the pass would have
needed interior mutability, which would have cost the compile-time guarantee
that nothing else is aliasing the tree mid-walk.

---

## 15. Associated functions: a method with no `self`

Rust distinguishes:

```rust
impl FixIndentation {
    fn new_indent(&self, /* … */) -> Indent { /* reads self.indent_width */ }

    fn align(first_fodder: &Fodder, old: Indent, line_up: usize) -> Indent { /* … */ }
}
```

`new_indent` is a **method** — called `self.new_indent(…)`. `align` is an
**associated function** — called `Self::align(…)`. It lives in the type's
namespace but takes no receiver. Go has no distinction; a Go method just ignores
its receiver.

**Why the split is there, and it is not arbitrary.** `align` and `align_strong`
genuinely never read any field: their "reset" branch returns the old indent
unchanged, so unlike `new_indent` they never consult the configured indent
width. Making them associated functions records that in the signature — and the
project's lint settings enforce it, because `clippy::unused_self` warns about a
method that ignores `self`.

So the shape of the API is telling you something true about the algorithm: two
of the four indent combinators cannot depend on configuration.

---

## 16. `Option<Box<T>>`, and the `as_deref` family

The tree is full of optional children:

```rust
pub expr2: Option<Box<Node>>,
```

`Box<Node>` is a `Node` on the heap (needed because a `Node` contains `Node`s —
without a `Box` the type would be infinitely large). `Option` makes it
optional. To get at it you almost never want `Option<Box<Node>>`; you want
`Option<&Node>` or `Option<&mut Node>`:

| you have | you want | write |
| --- | --- | --- |
| `&Option<Box<Node>>` | `Option<&Node>` | `field.expr2.as_deref()` |
| `&mut Option<Box<Node>>` | `Option<&mut Node>` | `field.expr2.as_deref_mut()` |

`as_ref()` would give you `Option<&Box<Node>>` — one indirection too many.
`as_deref` peels the `Box` as well.

And a pattern worth recognising, because this phase used it repeatedly:

```rust
let message_indent = field.expr2.as_deref().map_or(curr_indent, |expr2| {
    self.new_indent(expr2.opening_fodder(), curr_indent, self.column + 1)
});
```

`map_or(default, f)` means "if `Some`, apply `f`; if `None`, use `default`". It
is the whole of Go's

```go
var x T
if p != nil { x = f(p) }
```

in one expression, and crucially it is an *expression*, so the result can be
`let`-bound and the value cannot be accidentally read before it is set.

**Why it appears so much here:** the Go being ported dereferences these pointers
without checking, because its parser guarantees they are non-nil. Rust will not
let you skip the check, so every one of those sites becomes an `if let` or a
`map_or`. The upside is real: `src/pass.rs` documents this as one of three
deliberate departures, and notes that the behaviour is identical for every tree
the parser can actually build.

---

## 17. Why 141 hand-written *inputs* were fine but hand-written *answers* were not

Not a Rust feature, but the most transferable thing in this phase, and it has a
Rust consequence worth seeing.

Phase 2d wrote 141 new test cases before writing any of the three passes they
test. Every **input** was written by hand. Not one **expected answer** was: those
were generated by running the real Go formatter and recording what it did.

The one place a hand-written answer did sneak in, it was wrong:

```rust
// This assertion failed. It says a file ending in four newlines has a fodder
// element carrying two blank lines. It has no fodder element at all.
assert_eq!(blanks("1\n\n\n\n"), ["LineEnd:2"]);
```

The lexer measures the whitespace run and *then* notices it has hit the end of
the file, so it breaks before storing anything.

The Rust consequence: this is why several tests in this crate assert on the
**tree** rather than on the output text. For instance `fix_newlines.rs` has

```rust
fn breaks(input: &str) -> Vec<bool>   // "does each designated slot now start on a fresh line?"
```

instead of comparing formatted strings. Asserting the *postcondition* needs no
guess about how the fodder was composed; asserting the text does. When you
cannot generate an answer, assert the property rather than the output.

---

## 18. Replacing a value you only have a `&mut` to

Phase 2e was the first phase whose passes change the *shape* of the tree rather
than the comments in it. `FixParens` turns `((e))` into `(e)`;
`RemovePlusObject` turns `a + { b: 1 }` into `a { b: 1 }`; `AddPlusObject` turns
`e { }` into `e + { }` and sometimes wraps the result in parentheses. All three
have to take a node's children out of one shape and put them into a differently
shaped one.

Here is the Go, from `remove_plus_object.go`:

```go
*node = &ast.ApplyBrace{
    NodeBase: binary.NodeBase,
    Left:     binary.Left,
    Right:    rhs,
}
```

Go is moving pointers around, so there is nothing to it. The Rust equivalent
does not compile in the obvious form:

```rust
// Does not compile.
if let NodeKind::Binary(binary) = &mut node.kind {
    node.kind = NodeKind::ApplyBrace(ApplyBrace {
        left: binary.left,    // cannot move out of a borrow
        right: binary.right,
    });
}
```

Two separate problems. First, `binary.left` is a `Box<Node>` behind a `&mut`,
and you cannot *move* a value out of a borrow — that would leave the thing you
borrowed with a hole in it, and Rust has no concept of a half-initialised
value. Second, `node.kind` is already mutably borrowed by the `if let`, so you
cannot assign to it while the borrow is live.

The fix for both is the same, and it is a standard Rust move:

```rust
let NodeKind::Binary(binary) = std::mem::replace(&mut node.kind, NodeKind::LiteralNull)
else {
    unreachable!("just matched a Binary")
};
// `binary` is now OWNED, so its fields can be moved freely.
node.kind = NodeKind::ApplyBrace(ApplyBrace {
    left: binary.left,
    right: binary.right,
});
```

`std::mem::replace(dest, value)` puts `value` where `dest` was and hands you
back what used to be there, **by value**. There is never a hole: for one
instant `node.kind` holds `LiteralNull`, which is a perfectly valid `NodeKind`,
and then it holds the real answer. `std::mem::take(dest)` is the same thing when
the type implements `Default` — `Fodder` does, which is why
`std::mem::take(&mut node.fodder)` appears instead.

Three consequences worth internalising, because all three shape how these three
passes read:

1. **The placeholder is a real value, not a trick.** People coming from C++
   look for "move out and leave it invalid". Rust has no such state, so you
   supply something cheap and valid. `NodeKind::LiteralNull` is a unit variant:
   constructing one allocates nothing.
2. **You have to test the shape before you own it.** Owning the payload means
   `node.kind` no longer holds it, so there is no going back — you cannot look,
   decide against it, and put it down again without writing the put-back. So
   `remove_plus_object.rs` splits the decision into
   `Self::is_implicit_plus_candidate(node)`, which only *borrows*, and does the
   `mem::replace` after it has answered yes. Go asks all three questions inside
   nested `if`s on a pointer it never gives up.
3. **Nesting needs the `Box` deref.** `FixParens` has to reach a
   *grand*child — `outer.inner` is a `Box<Node>`, and the Parens inside it has
   its own `inner`. Once `outer` is owned, `let inner_node = *outer.inner;`
   moves the `Node` out of the box. `*` on a `Box` you own is a move; `*` on a
   `&Box` is a borrow. That distinction is the whole difference between the code
   compiling and not.

Sequencing tip: pull every piece you need out into `let` bindings *before* you
start reassembling. `fix_parens.rs` takes the inner node's fodder and its close
fodder into locals, then rebuilds, then does the fodder moves. Trying to do it
in upstream's order means borrowing the half-built node.

---

## 19. Pointer identity, and the one Go trick that has no Rust translation

This is 2e's real design problem, and the most interesting thing in the phase.

`AddPlusObject` has to decide whether `e { }` becoming `e + { }` needs
parentheses, which depends on *where in the parent* the node sits. `f() {a:1}`
as a call target needs them; `f({a:1} {b:2})` as an argument does not. Upstream
carries the parent node along the walk and asks:

```go
case *ast.Apply:
    if parent.Target == *node {
        needsParens = true
    }
```

Read that twice, because it is stranger than it looks. `node` is a
`*ast.Node` — a pointer to the slot the walk is currently in. When the walk came
through the target slot, `node` *is* `&parent.Target`, so `parent.Target` and
`*node` are the same pointer and the comparison is true. When the walk came
through an argument, they are different pointers and it is false. The condition
is not really "is the parent's target this expression" — it is **"did the walk
arrive through the target slot"**, answered by comparing a place against itself.

Rust cannot write that, for two reasons that are both worth knowing:

- **The parent is mutably borrowed for the whole of its own traversal.** While
  `pass.apply(node)` is running, there is exactly one live `&mut Apply`, and the
  borrow checker guarantees no second reference to it exists. So there is
  nothing to compare against.
- **`Node` has no identity apart from its address.** It derives `PartialEq`, so
  `==` compares *contents* — and two structurally identical subtrees would
  compare equal, which is the wrong answer. (`std::ptr::eq` exists and compares
  addresses, but you cannot get two references to the parent to hand it.)

So the port carries a **descriptor** instead of the parent:

```rust
pub enum Parent {
    None,
    Other,
    ApplyTarget,
    IndexTarget,
    InSuperIndex,
    BinaryLeft(BinaryOp),
    BinaryRight(BinaryOp),
    UnaryOperand,
}
```

Each variant names the slot the walk came through, and the pass fills it in when
it visits that slot. The pointer comparison becomes a `match`. Two general
lessons in that:

1. **Where Go asks a question about identity, Rust usually asks it about
   provenance instead.** Go could be vague about how the walk got here, because
   it could check afterwards. Rust makes you say it on the way down. The result
   is arguably clearer than the original: `BinaryLeft(op)` and
   `BinaryRight(op)` being separate variants puts the associativity rule — `+`
   on the left of `+` is fine, on the right it is not — right in the type.
2. **The cost is real and has to be paid somewhere.** `pass::base::apply` hands
   *one* `ctx` to every slot it visits, so a pass that needs different contexts
   per slot cannot call it. `AddPlusObject` therefore restates five of the base
   traversals by hand. That is a maintenance hazard with no compiler check
   behind it: an override that forgets a slot silently stops doing its job
   there. The guard is testing — a case per slot, plus a *separate* walk over
   `pass::base` that shares none of the overrides and can therefore still see
   what one of them skipped.

The second lesson generalises past this codebase. When you replace a dynamic
check with a statically-carried descriptor, you move a runtime question into
the type — and you also move the risk from "the check is wrong" to "the
descriptor was never set". Those need different tests.

---

## 20. Rebuilding a linked structure you own, back to front

Phase 2f's pass, `SortImports`, is the first one that does not edit the tree at
all — it **throws a piece of it away and builds a new one**. Given

```
local b = import 'b';
local a = import 'a';
1
```

it produces a fresh chain: a `Local` holding `a`, whose body is a `Local`
holding `b`, whose body is the `1`. Go does this in four lines, because Go is
moving pointers and does not mind that the thing it is reading from is also the
thing it is writing to.

The Rust problem is not the recursion. It is that **each new node needs a piece
of the element next to it**, and you are consuming the elements as you go.
Every import carries the fodder — the comments and blank lines — that followed
it in the source, and the rebuild puts element `i - 1`'s fodder in *front* of
element `i`. So while you are building the node for element 5, you need to take
something out of element 4.

The obvious shape does not work:

```rust
// Does not compile.
for i in (0..elems.len()).rev() {
    let fodder = elems[i - 1].adjacent_fodder;   // cannot move out of an index
    body = Node::new(fodder, Local { bind: elems[i].bind, body: Box::new(body) });
}
```

Indexing gives you a *borrow* of the vector's element, and §18's rule applies:
you cannot move a field out of a borrow. You could clone the fodder, or leave
placeholders behind with `std::mem::take`, and both work. But there is a shape
that needs neither, and it is worth knowing because it comes up whenever you
consume a sequence pairwise:

```rust
while let Some(elem) = self.0.pop() {
    let fodder = match self.0.last_mut() {
        Some(previous) => std::mem::take(&mut previous.adjacent_fodder),
        None => std::mem::take(&mut group_open_fodder),
    };
    body = Node::new(fodder, /* … */ Local { binds: vec![elem.bind], body: Box::new(body) });
}
```

`Vec::pop` hands you the last element **by value** — the vector is now one
shorter and has no claim on it, so `elem.bind` moves freely. And because you
are walking backwards, the element you need to read from is now exactly
`last_mut()`: the new last. You take its fodder, and on the next iteration you
pop that same element, so nothing is left half-emptied that anyone will look at
again.

Three things to take from this:

1. **`pop` is the move-friendly end of a `Vec`.** `v[i]` borrows, `v.pop()`
   owns. If an algorithm can be written to consume from the back, ownership
   stops being a fight. (`Vec::drain` and `into_iter` are the other two ways
   out.)
2. **Walking backwards can be an ownership decision, not just an algorithmic
   one.** Go walks backwards here because it is building a linked list and
   needs the tail first. Rust wants to walk backwards for a second, unrelated
   reason: the neighbour it needs to read is the one it is about to consume.
   The two reasons happen to agree, which is luck, but noticing *why* it works
   is what lets you find the shape next time they do not.
3. **`std::mem::take` on a field of something that is about to be dropped is
   free and honest.** It is not a hack to dodge the borrow checker; it is a
   statement that this fodder now lives somewhere else. `Fodder` implements
   `Default`, so `take` leaves an empty one — and an empty fodder is a
   perfectly valid fodder, exactly as §18's `LiteralNull` is a perfectly valid
   `NodeKind`.

There is a fourth thing, which is about recursion rather than ownership.
`SortImports` recurses twice for different reasons and the two have different
shapes:

```rust
fn absorb(mut self, local: Local, group_open_fodder: Fodder) -> Node {
    // … accumulate this local's imports into `self` …
    if !body.ends_an_import_group() {
        return self.absorb(next, group_open_fodder);   // same group, carried on
    }
    // …
    let body_after_group = if body.is_import_group_local() {
        Group::new().absorb(next, next_open_fodder)    // a NEW group
    } else { /* … */ };
    self.build(body_after_group, group_open_fodder)
}
```

The method takes `self` **by value**, not `&mut self`. That is what makes the
first branch a clean tail call: the group is handed onward and this frame has
no further claim on it. Had it been `&mut self`, the recursive call would
borrow `self` for the duration and the `return` would be fine but
`self.build(...)` at the end — which *consumes* the group — would not compile.
Taking `self` by value in a recursive method is the Rust way of saying "this
frame is done with it either way": either it is passed on, or it is consumed
here.

---

## Quiz

No looking. Answers below.

1. What is the difference between a Rust enum and a Go enum (or a C one)?
2. Why does `NodeKind` being an enum make it safe to add a 29th kind of AST
   node?
3. Rust has no null. What replaces it, and what are its two variants?
4. How many `&mut` references to one value may exist at the same time? How many
   `&T`?
5. This does not compile. Why, and what is the fix?
   ```rust
   field.expr1.fodder.move_front(&mut field.fodder1);
   ```
6. What does `..` mean in `let ObjectField { fodder1, .. } = field;`?
7. What can a trait method have that a Go interface method cannot?
8. Rust has no `super`. What did this codebase do instead, and in which file
   is the reasoning written down?
9. What does `?` do at the end of a fallible call?
10. You are writing a function that takes some text and returns some text.
    Which four types are in play, and which two should be the parameters?
11. What is "fodder" in this crate, and why does a formatter need it when a
    compiler does not?
12. Why does this project generate its test expectations from Go programs
    instead of writing them by hand?
13. A formatter pass changes an AST, but the file's formatted output does not
    change. How is that possible?
14. Rewrite this Go in Rust without an index or a length check:
    ```go
    if len(xs) == 1 { use(&xs[0]) }
    ```
15. `let s = String::from("héllo");`. What does `s[1]` do? What does `&s[1..]`
    do? What should you write instead if you only want to know whether `s`
    starts with `h`?
16. `EnforceCommentStyle` derives `Clone` but not `Copy`, although `Copy` would
    compile. Why is leaving it off the safer choice?
17. What does `#[derive(Debug)]` buy you that you would miss immediately in a
    test?
18. Question 5 said this does not compile. Why does *this* compile, then?
    ```rust
    self.fill(&mut field.op_fodder, true, true, indent);
    if let Some(expr3) = field.expr3.as_deref_mut() { self.visit(expr3, indent, true); }
    ```
19. `fn fill(&mut self, fodder: &mut Fodder)` — that is two `&mut` in one
    signature. Why is it not a borrow error at the call site?
20. What is the difference between a method and an associated function, and
    what does the choice tell a reader about `FixIndentation::align`?
21. You have a `&mut Option<Box<Node>>` and you want an `Option<&mut Node>`.
    Which method? What would `as_mut()` have given you instead?
22. Rewrite this Go as one Rust expression:
    ```go
    x := fallback
    if p != nil { x = f(p) }
    ```
23. Phase 2d wrote 141 test cases by hand before writing the code they test,
    and that was correct. It also wrote one expected *answer* by hand, and
    that was wrong. What is the rule?
24. A test wants to check that a pass put a newline in the right places. Why
    might asserting on the formatted text be the worse choice than asserting
    on the tree?
25. Why does this not compile, and what one function call fixes it?
    ```rust
    if let NodeKind::Binary(binary) = &mut node.kind {
        node.kind = NodeKind::ApplyBrace(ApplyBrace { left: binary.left, right: binary.right });
    }
    ```
26. `std::mem::replace` needs a value to leave behind. Why can you not leave
    "nothing" behind, and what is left behind in these passes?
27. Go decides which slot of a parent the walk came through by writing
    `parent.Target == *node`. Why can Rust not write that, and what replaces
    it? Name *two* reasons, not one.
28. `AddPlusObject` restates five of `pass::base`'s traversals by hand instead
    of calling them. Why is it forced to, and what is the risk that creates?
29. You are consuming a `Vec` back to front, and each element needs a field
    taken out of the element *before* it. Why does indexing not work, and what
    two-line shape makes it work with no clone and no placeholder left behind
    for anyone to see?
30. `Group::absorb` is recursive and takes `self` by value rather than
    `&mut self`. What would stop compiling if it took `&mut self`?
31. Rust's `sort_by` and Go's `sort.Slice` both sort correctly. Why can this
    codebase not use `sort_by`, and what does that tell you about when a Rust
    crate is and is not a substitute for the Go library it mirrors?

---

## Answers

1. A Rust enum variant can **carry data**, and different variants can carry
   different data. A C or Go enum is just a set of named integers. This makes
   a Rust enum the natural way to say "exactly one of these shapes".
2. Because `match` must handle every variant. Adding one breaks compilation
   everywhere it needs handling, so the compiler hands you the to-do list
   instead of letting a case slip through at runtime.
3. `Option<T>`, with variants `Some(value)` and `None`.
4. Exactly one `&mut`, and while it exists, no `&T` at all. Many `&T` at once
   is fine.
5. Two `&mut` borrows of `field` would be alive simultaneously. Fix:
   destructure — `let ObjectField { fodder1, expr1, .. } = &mut *field;` —
   so the compiler sees two independent fields rather than two borrows of the
   whole struct.
6. "And ignore the remaining fields." Without it you would have to name every
   field of the struct.
7. A **default body**, so implementors inherit real behaviour and not just a
   signature. `AstPass` has ~30 defaults and a pass overrides one or two.
8. The "walk deeper" logic went into plain functions in a `base` module, called
   both by the trait's default methods and by an overriding pass at the point
   where Go would write `c.Base.Array(p, node, ctx)`. Written up at the top of
   `crates/rtk-jsonnetfmt/src/pass.rs`.
9. On `Err`, returns that error from the enclosing function immediately. On
   `Ok`, unwraps the value and continues.
10. `String`, `&str`, and for the return `Result<String, …>`. Parameters should
    be `&str`; return an owned `String`.
11. Comments and blank lines, carried along attached to the tree. A compiler
    discards them because they do not affect meaning; a formatter must
    reproduce them, so they are part of the data structure.
12. Because reading a reference implementation and writing down what you think
    it does was measured and found unreliable — 2 of 16 hand-derived
    expectations were wrong, and a careful reading of another Go library
    shipped nine bugs. Generated oracles remove the guessing.
13. Other passes that would have finished the job do not exist yet, so the
    difference is masked in the final text comparison. Two of the 138 corpus
    files are in exactly that state.
14. A **slice pattern**: `if let [x] = xs.as_mut_slice() { use(x) }`. It
    matches only a one-element slice and names that element, so the length
    check and the access are one expression.
15. `s[1]` does not compile — a `str` cannot be indexed by a single number,
    because "byte 1" and "character 1" are different questions. `&s[1..]`
    compiles but **panics at runtime**, because byte 1 is inside the two-byte
    `é`. Write `s.starts_with('h')`.
16. Because the struct carries state. `seen_first_fodder` flips once the
    traversal has seen the first comment, and that is what keeps a `#!`
    hashbang line intact. With `Copy`, an accidental assignment would silently
    make a second pass with its own copy of the flag and no compile error;
    without it, the assignment moves the pass and any later use of the original
    fails to compile.
17. Printing with `{:?}` — which is what `assert_eq!` uses to show you the two
    values when it fails. Without `Debug` the assertion does not compile.
18. Because the borrow checker tracks **places**, not values.
    `field.op_fodder` and `field.expr3` are different paths, so borrowing one
    says nothing about the other. Question 5's version borrowed `field`
    *itself* twice, which is one place — hence the destructuring.
19. Because `self` and `fodder` are unrelated values. `self` is the pass,
    holding a column counter; `fodder` is part of the syntax tree. A pass
    feels like it is "inside" the tree it walks, but it is a separate object
    holding a cursor.
20. A method takes a `self` receiver and is called `x.f()`; an associated
    function takes none and is called `Type::f()`. `align` being an
    associated function says it cannot read any configuration — and that is
    true of the algorithm: its reset branch returns the old indent unchanged,
    so it never consults the indent width. `clippy::unused_self` enforces the
    honesty.
21. `as_deref_mut()`. `as_mut()` would give `Option<&mut Box<Node>>` — one
    indirection too many, because it peels the `Option` but not the `Box`.
22. `let x = p.as_deref().map_or(fallback, |p| f(p));` — and the win over the
    Go is that it is an expression, so `x` cannot be read before it is set.
23. **Inputs by hand, answers from an oracle.** Deciding *what* to test is
    judgement and a person is good at it; predicting what a reference
    implementation does is recall and this project has measured itself
    unreliable at it — 2 of 16 hand-derived expectations were wrong, both
    about fodder the model composes rather than reads.
24. Because the text depends on how the *lexer* composed the fodder, which is
    a separate question you would then also be guessing at — and on passes
    that may not be written yet. Asserting the pass's own postcondition
    ("does this slot now start on a fresh line?") needs neither. When you
    cannot generate the answer, assert the property rather than the output.
25. Two problems at once. `binary.left` is a `Box<Node>` behind a `&mut`, and
    you cannot move a value out of a borrow; and `node.kind` is already
    mutably borrowed by the `if let`, so it cannot be assigned to. Both go
    away with `std::mem::replace(&mut node.kind, NodeKind::LiteralNull)`,
    which hands you the payload **owned** and leaves a valid placeholder
    behind. `std::mem::take` is the same thing for a type with a `Default`.
26. Because Rust has no half-initialised state — every place always holds a
    valid value of its type, which is what makes moving out of a borrow
    illegal in the first place. These passes leave `NodeKind::LiteralNull`, a
    unit variant that allocates nothing. It exists for one or two statements
    and nothing observes it.
27. First, the parent is mutably borrowed for the whole of its own traversal,
    so no second reference to it exists to compare against. Second, `Node`
    derives `PartialEq`, so `==` compares *contents* — two structurally
    identical subtrees would compare equal, which is the wrong answer
    entirely. What replaces it is `passes::add_plus_object::Parent`, a
    descriptor naming the slot the walk arrived through, set on the way *down*
    rather than checked on arrival. The pointer comparison becomes a `match`.
28. Because `pass::base::apply` takes one `ctx` and hands it to every slot it
    visits, and this pass needs a different context per slot — the target of a
    call needs parens, an argument does not. The risk is that a restated
    traversal is a copy with no compiler check behind it: drop a slot and the
    pass silently stops working there, with nothing in the pass itself looking
    wrong. Hence a test case per slot, plus a separate counting walk over
    `pass::base` that shares none of the overrides.
29. `v[i]` gives a *borrow* of the element, and you cannot move a field out of
    a borrow (§18). The shape is `while let Some(elem) = self.0.pop()` plus
    `self.0.last_mut()`: `pop` hands the element over by value, and because
    you are going backwards the element you need to read from is now the new
    last. You `std::mem::take` its field, and it is popped on the very next
    iteration, so nothing half-emptied is ever looked at again.
30. `self.build(...)` on the last line, which *consumes* the group. A
    `&mut self` method only ever has a borrow, and you cannot consume through
    one. Taking `self` by value says "this frame is finished with it either
    way": either it is handed to the recursive call, or it is consumed here.
31. Because sorting has more than one right answer, and only one of them is
    the one `tk fmt` writes to the file. Two imports can have the same path,
    "sorted" does not say which of them comes first, and the two languages
    disagree — Go's leaves `i09 i01 i04` where Rust's stable sort leaves
    `i01 i04 i09`. Both are sorted. So `src/go_sort.rs` ports Go's pdqsort.
    The general lesson: a crate is a substitute for the Go library it mirrors
    only where the *observable* behaviour is fully specified by what both
    claim to do. Wherever the behaviour is unspecified — tie order here,
    separator handling in `rtk-gobwas-glob`, what a bare `1.2.3` means in
    `rtk-masterminds` — the two will differ, and "correct" is not the bar.

---

## Change log

- **2026-09-16** — Created after Phase 2b of the `rtk fmt` port (the pass
  traversal plus `FixTrailingCommas`, `NoRedundantSliceColon`,
  `PrettyFieldNames`). Covers: the formatter's shape, structs and enums,
  `Option`, ownership and borrowing with the destructuring trick, traits and
  the missing `super`, `Result` and `?`, `String`/`&str`, where tests live, and
  the generated-oracle testing idea. Quiz: 13 questions.
- **2026-09-16** — Phase 2c (`EnforceStringStyle`, `EnforceCommentStyle`).
  Added §10 slice patterns, §11 why `&s[2..]` can panic and what to write
  instead, §12 `derive` and the `Copy` that was deliberately left off. Quiz:
  17 questions. The §1 table of implemented passes is left as it was, per the
  append-don't-rewrite rule; `crates/rtk-jsonnetfmt/src/passes/mod.rs` is the
  current list.
- **2026-09-16** — Phase 2d (`EnforceMaxBlankLines`, `FixNewlines`,
  `FixIndentation`, and the two four-line pipeline steps). Added §13 disjoint
  field borrows — which is the proper answer to the question §4 and quiz Q5
  left half-told — §14 why `&mut self` and `&mut arg` coexist, §15 methods
  against associated functions and what `align` not taking `self` tells you,
  §16 `Option<Box<T>>` and the `as_deref` family with `map_or`, and §17 the
  inputs-by-hand / answers-from-an-oracle rule and its consequence for what a
  test should assert. Quiz: 24 questions. §1's table of implemented passes is
  again left as it was, per the append-don't-rewrite rule.
- **2026-09-16** — Phase 2e (`FixParens`, `RemovePlusObject`, `AddPlusObject`:
  the three passes that change what a file evaluates to rather than how it
  looks). Added §18 replacing a value behind a `&mut` — `std::mem::replace`,
  why there is no "move out and leave it invalid", why the shape has to be
  tested before the payload is owned, and moving a `Node` out of a `Box` you
  own — and §19 pointer identity, which is 2e's whole design problem: Go
  decides which slot of a parent the walk came through by comparing a place
  against itself, Rust cannot, and the replacement is a descriptor set on the
  way down. §19 is the one to read if only one gets read; it generalises past
  this codebase. Quiz: 28 questions. §1's table of implemented passes is left
  as it was again; `crates/rtk-jsonnetfmt/src/passes/mod.rs` is the current
  list.
- **2026-09-16** — Phase 2f (`SortImports`, the twelfth and last pass, plus a
  port of Go's `sort.Slice`). Added §20 rebuilding an owned linked structure
  by recursion: why indexing fails where §18's rule bites a second time, why
  `Vec::pop` plus `last_mut()` is the shape that needs neither a clone nor a
  visible placeholder, why walking backwards can be an *ownership* decision
  rather than an algorithmic one, and why a recursive method that will consume
  its state has to take `self` by value. Quiz: 31 questions, and Q31 is the
  one worth arguing with — it is about when a Rust crate is not a substitute
  for the Go library it mirrors, which is the reason three crates here are
  hand ports. §1's table of implemented passes is left as it was, per the
  append-don't-rewrite rule; that pipeline is now complete and
  `crates/rtk-jsonnetfmt/src/lib.rs`'s `format` doc carries all fourteen
  steps.
