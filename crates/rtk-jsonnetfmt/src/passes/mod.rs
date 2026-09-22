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
//! module. That leaves `SortImports` (Phase 2f), `FixParens` and the two
//! `*PlusObject` passes (Phase 2e), and the three strip passes, which no
//! phase schedules because `Options::default` skips them.
//!
//! [`Node::remove_initial_newlines`]: crate::ast::Node::remove_initial_newlines
//! [`Fodder::remove_extra_trailing_newlines`]: crate::fodder::Fodder::remove_extra_trailing_newlines
//!
//! # None of these has a context
//!
//! All seven use `Ctx = ()`. Only `AddPlusObject` — Phase 2e, and skipped
//! under `Options::default` — needs one.
//!
//! # Three of them read an option, and one of them has state
//!
//! Upstream gives a pass the whole `Options` struct; the three that need one
//! take the single field they read, which is what says what the pass can
//! depend on. [`EnforceCommentStyle`] additionally carries `seenFirstFodder`
//! across the traversal, so it is constructed per file and is deliberately not
//! `Copy` — see its own documentation. Whether a pass runs at all is
//! [`crate::format`]'s business, exactly as it is `FormatNode`'s.

pub mod enforce_comment_style;
pub mod enforce_max_blank_lines;
pub mod enforce_string_style;
pub mod fix_newlines;
pub mod fix_trailing_commas;
pub mod no_redundant_slice_colon;
pub mod pretty_field_names;

pub use enforce_comment_style::EnforceCommentStyle;
pub use enforce_max_blank_lines::EnforceMaxBlankLines;
pub use enforce_string_style::EnforceStringStyle;
pub use fix_newlines::FixNewlines;
pub use fix_trailing_commas::FixTrailingCommas;
pub use no_redundant_slice_colon::NoRedundantSliceColon;
pub use pretty_field_names::PrettyFieldNames;
