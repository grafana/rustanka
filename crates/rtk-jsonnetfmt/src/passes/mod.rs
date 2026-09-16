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
//! [`NoRedundantSliceColon`] and [`PrettyFieldNames`]. Phase 2c so far:
//! [`EnforceStringStyle`]. The other eight are still to come, which is why
//! `quarantine.toml` and `testdata/corpus-baseline.toml` still carry entries.
//!
//! # None of these four has a context
//!
//! All four use `Ctx = ()`. Only `AddPlusObject` — Phase 2e, and skipped
//! under `Options::default` — needs one.
//!
//! # One of them reads an option
//!
//! Upstream gives a pass the whole `Options` struct; [`EnforceStringStyle`]
//! takes the single field it reads, which is what says what the pass can
//! depend on. Whether a pass runs at all is [`crate::format`]'s business,
//! exactly as it is `FormatNode`'s.

pub mod enforce_string_style;
pub mod fix_trailing_commas;
pub mod no_redundant_slice_colon;
pub mod pretty_field_names;

pub use enforce_string_style::EnforceStringStyle;
pub use fix_trailing_commas::FixTrailingCommas;
pub use no_redundant_slice_colon::NoRedundantSliceColon;
pub use pretty_field_names::PrettyFieldNames;
