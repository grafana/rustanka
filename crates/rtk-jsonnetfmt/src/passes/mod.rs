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
//! [`EnforceStringStyle`] and [`EnforceCommentStyle`]. The other seven are
//! still to come, which is why `quarantine.toml` and
//! `testdata/corpus-baseline.toml` still carry entries.
//!
//! # None of these five has a context
//!
//! All five use `Ctx = ()`. Only `AddPlusObject` — Phase 2e, and skipped
//! under `Options::default` — needs one.
//!
//! # Two of them read an option, and one of them has state
//!
//! Upstream gives a pass the whole `Options` struct; the two that need one
//! take the single field they read, which is what says what the pass can
//! depend on. [`EnforceCommentStyle`] additionally carries `seenFirstFodder`
//! across the traversal, so it is constructed per file and is deliberately not
//! `Copy` — see its own documentation. Whether a pass runs at all is
//! [`crate::format`]'s business, exactly as it is `FormatNode`'s.

pub mod enforce_comment_style;
pub mod enforce_string_style;
pub mod fix_trailing_commas;
pub mod no_redundant_slice_colon;
pub mod pretty_field_names;

pub use enforce_comment_style::EnforceCommentStyle;
pub use enforce_string_style::EnforceStringStyle;
pub use fix_trailing_commas::FixTrailingCommas;
pub use no_redundant_slice_colon::NoRedundantSliceColon;
pub use pretty_field_names::PrettyFieldNames;
