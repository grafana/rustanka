//! The formatter passes, one module per file of `internal/formatter`.
//!
//! `FormatNode` runs these in a fixed order, and the order is read off
//! upstream rather than inferred — see [`crate::format`]. Each is an
//! implementation of [`crate::pass::AstPass`] that overrides one or two hooks
//! and delegates the rest to `pass::base`.
//!
//! # What has landed
//!
//! Phase 2b of `docs/rtk-fmt-plan.md`: [`FixTrailingCommas`],
//! [`NoRedundantSliceColon`] and [`PrettyFieldNames`]. Phase 2c:
//! [`EnforceStringStyle`] and [`EnforceCommentStyle`]. Phase 2d:
//! [`EnforceMaxBlankLines`] and [`FixNewlines`], plus the two four-line
//! pipeline steps [`Node::remove_initial_newlines`] and
//! [`Fodder::remove_extra_trailing_newlines`], which live on the types they
//! mutate. `FixIndentation` is 2d's other half and is deliberately **not**
//! here: `FormatNode` calls `visitor.VisitFile(node, finalFodder)` on it
//! directly rather than through the `pass.ASTPass` machinery, so it is not an
//! implementation of [`crate::pass::AstPass`] and does not belong in this
//! module. Phase 2e: [`FixParens`], [`RemovePlusObject`] and
//! [`AddPlusObject`]. Phase 2f: `SortImports`, which is not a visitor and so
//! is not here either. The three strip passes are **not ported at all**, and
//! [`crate::Options`] carries the decision and the route back.
//!
//! # Phase 2e's three are the ones that change meaning
//!
//! Everything before them is cosmetic: a bug moves a comment or an indent. A
//! bug in these three changes what a file *evaluates to* —
//! `{ a: 1 } { b: 2 }.a` has to become `({ a: 1 } + { b: 2 }).a`, and without
//! the inserted parentheses the formatted expression is a runtime error rather
//! than a differently-spelled program. They are also the first three that
//! replace a node rather than rewriting its fodder, which is why each one
//! needs `std::mem::replace` to get an owned payload out of `node.kind`:
//! moving a child into a differently-shaped parent is not something a `&mut`
//! borrow of the whole node will allow.
//!
//! [`AddPlusObject`] is the `use_implicit_plus: false` branch of step 7 and so
//! is the one pass here that `tk fmt` never runs; [`RemovePlusObject`] is the
//! branch it does.
//!
//! [`Node::remove_initial_newlines`]: crate::ast::Node::remove_initial_newlines
//! [`Fodder::remove_extra_trailing_newlines`]: crate::fodder::Fodder::remove_extra_trailing_newlines
//!
//! # Only one of these has a context
//!
//! Nine of the ten use `Ctx = ()`. [`AddPlusObject`] is the exception, and it
//! is the reason [`crate::pass::AstPass::Ctx`] is an associated type rather
//! than `()`: it carries [`add_plus_object::Parent`], a descriptor of the
//! parent refined per slot, because Go's version compares the parent's child
//! *pointer* against the current node and Rust has no answer to that while the
//! parent is mutably borrowed. It is also the only pass that overrides more
//! than two hooks, for the same reason — the per-slot refinement has to happen
//! in the hooks, since `pass::base` gives one context to every slot.
//!
//! # Three of them read an option, and one of them has state
//!
//! Upstream gives a pass the whole `Options` struct; the three that need one
//! take the single field they read, which is what says what the pass can
//! depend on. [`EnforceCommentStyle`] additionally carries `seenFirstFodder`
//! across the traversal, so it is constructed per file and is deliberately not
//! `Copy` — see its own documentation. Whether a pass runs at all is
//! [`crate::format`]'s business, exactly as it is `FormatNode`'s.

pub mod add_plus_object;
pub mod enforce_comment_style;
pub mod enforce_max_blank_lines;
pub mod enforce_string_style;
pub mod fix_newlines;
pub mod fix_parens;
pub mod fix_trailing_commas;
pub mod no_redundant_slice_colon;
pub mod pretty_field_names;
pub mod remove_plus_object;

pub use add_plus_object::AddPlusObject;
pub use enforce_comment_style::EnforceCommentStyle;
pub use enforce_max_blank_lines::EnforceMaxBlankLines;
pub use enforce_string_style::EnforceStringStyle;
pub use fix_newlines::FixNewlines;
pub use fix_parens::FixParens;
pub use fix_trailing_commas::FixTrailingCommas;
pub use no_redundant_slice_colon::NoRedundantSliceColon;
pub use pretty_field_names::PrettyFieldNames;
pub use remove_plus_object::RemovePlusObject;
