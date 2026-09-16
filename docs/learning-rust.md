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
