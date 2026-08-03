use std::{
	any::Any,
	cell::{Cell, RefCell},
	clone::Clone,
	cmp::Reverse,
	collections::hash_map::Entry,
	fmt::{self, Debug},
	hash::{Hash, Hasher},
	num::Saturating,
	ops::ControlFlow,
};

use educe::Educe;
use jrsonnet_gcmodule::{Acyclic, Cc, Trace, Weak, cc_dyn};
use jrsonnet_interner::IStr;
use jrsonnet_ir::Span;
use rustc_hash::{FxHashMap, FxHashSet};

mod oop;
mod static_shape;

pub use jrsonnet_ir::Visibility;
pub use oop::ObjValueBuilder;
pub use static_shape::{ObjShape, ObjShapeBuilder, ShapeField, StaticShapeOopObject};

use crate::{
	CcUnbound, MaybeUnbound, Result, Thunk, Unbound, Val,
	arr::{PickObjectKeyValues, PickObjectValues, arridx},
	bail,
	error::{ErrorKind::*, suggest_object_fields},
	evaluate::operator::evaluate_add_op,
	identity_hash,
	val::{ArrValue, ThunkValue},
};

#[cfg(not(feature = "exp-preserve-order"))]
pub mod ordering {
	#![allow(
		// This module works as stub for preserve-order feature
		clippy::unused_self,
	)]

	use jrsonnet_gcmodule::Trace;

	#[derive(Clone, Copy, Default, Debug, Trace, PartialEq, Eq, PartialOrd, Ord)]
	pub struct FieldIndex(());
	impl FieldIndex {
		pub fn absolute(_v: u32) -> Self {
			Self(())
		}
		#[must_use]
		pub const fn next(self) -> Self {
			Self(())
		}
	}

	#[derive(Clone, Copy, Default, Debug, Trace, PartialEq, Eq, PartialOrd, Ord)]
	pub struct SuperDepth(());
	impl SuperDepth {
		pub(super) fn deepen(self) {}
	}
}

#[cfg(feature = "exp-preserve-order")]
pub mod ordering {
	use jrsonnet_gcmodule::Trace;

	#[derive(Clone, Copy, Default, Debug, Trace, PartialEq, Eq, PartialOrd, Ord)]
	pub struct FieldIndex(u32);
	impl FieldIndex {
		pub fn absolute(v: u32) -> Self {
			Self(v)
		}
		#[must_use]
		pub fn next(self) -> Self {
			Self(self.0 + 1)
		}
	}

	#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
	pub struct SuperDepth(u32);
	impl SuperDepth {
		pub(super) fn deepen(&mut self) {
			self.0 += 1;
		}
	}
}

use ordering::{FieldIndex, SuperDepth};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct FieldSortKey(Reverse<SuperDepth>, FieldIndex);
impl FieldSortKey {
	pub fn new(depth: SuperDepth, index: FieldIndex) -> Self {
		Self(Reverse(depth), index)
	}
}

// 0 - add
//  12 - visibility
#[derive(Clone, Copy, Acyclic)]
pub struct ObjFieldFlags(u8);
impl ObjFieldFlags {
	pub fn new(add: bool, visibility: Visibility) -> Self {
		let mut v = 0;
		if add {
			v |= 1;
		}
		v |= match visibility {
			Visibility::Normal => 0b000,
			Visibility::Hidden => 0b010,
			Visibility::Unhide => 0b100,
		};
		Self(v)
	}
	pub fn add(&self) -> bool {
		self.0 & 1 != 0
	}
	pub fn visibility(&self) -> Visibility {
		match (self.0 & 0b110) >> 1 {
			0b00 => Visibility::Normal,
			0b01 => Visibility::Hidden,
			0b10 => Visibility::Unhide,
			_ => unreachable!(),
		}
	}
}
impl Debug for ObjFieldFlags {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("ObjFieldFlags")
			.field("add", &self.add())
			.field("visibility", &self.visibility())
			.finish()
	}
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Trace)]
pub struct ObjMember {
	flags: ObjFieldFlags,
	pub invoke: MaybeUnbound,
	pub location: Option<Span>,
}

cc_dyn!(CcObjectAssertion, ObjectAssertion, pub fn new() {...});
pub trait ObjectAssertion: Trace {
	fn run(&self, sup_this: SupThis) -> Result<()>;
}

// Field => This

#[derive(Trace, Debug)]
enum CacheValue {
	Cached(Result<Option<Val>>),
	Pending,
}

pub type EnumFieldsHandler<'a> =
	dyn FnMut(SuperDepth, FieldIndex, IStr, EnumFields) -> ControlFlow<()> + 'a;

#[derive(Debug)]
pub enum EnumFields {
	Normal(Visibility),
	Omit(Skip),
}

#[derive(Trace, Clone)]
pub enum GetFor {
	// Return value
	Final(Val),
	// Continue iterating over cores, add current value to sum stack
	SuperPlus(Val),
	// Ignore the field value, stop at this layer instead
	Omit(#[trace(skip)] Skip),
	NotFound,
}

#[derive(Acyclic, Clone)]
pub enum FieldVisibility {
	Found(Visibility),
	Omit(Skip),
	NotFound,
}

#[derive(Acyclic, Clone)]
pub enum HasFieldIncludeHidden {
	Exists,
	NotFound,
	Omit(Skip),
}

type Skip = Saturating<usize>;

pub trait ObjectCore: Trace + Any + Debug {
	// If callback returns false, iteration stops, and this call returns false.
	fn enum_fields_core(
		&self,
		super_depth: &mut SuperDepth,
		handler: &mut EnumFieldsHandler<'_>,
	) -> bool;

	fn has_field_include_hidden_core(&self, name: IStr) -> HasFieldIncludeHidden;

	fn get_for_core(&self, key: IStr, sup_this: SupThis, omit_only: bool) -> Result<GetFor>;
	fn field_visibility_core(&self, field: IStr) -> FieldVisibility;

	fn run_assertions_core(&self, sup_this: SupThis) -> Result<()>;

	fn has_assertion(&self) -> bool {
		false
	}
}

#[derive(Clone, Trace)]
pub struct WeakObjValue(#[trace(skip)] Weak<ObjValueInner>);
impl WeakObjValue {
	/// Returns `true` if the referenced object is still alive.
	pub fn is_alive(&self) -> bool {
		self.0.upgrade().is_some()
	}
	/// Attempts to obtain a strong reference. Returns `None` if the object
	/// has been collected.
	pub fn upgrade(&self) -> Option<ObjValue> {
		self.0.upgrade().map(ObjValue)
	}
}
impl Debug for WeakObjValue {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("WeakObjValue").finish()
	}
}

impl PartialEq for WeakObjValue {
	fn eq(&self, other: &Self) -> bool {
		Weak::ptr_eq(&self.0, &other.0)
	}
}

impl Eq for WeakObjValue {}
impl Hash for WeakObjValue {
	fn hash<H: Hasher>(&self, hasher: &mut H) {
		// Safety: usize is POD
		let addr = unsafe { *std::ptr::addr_of!(self.0).cast() };
		hasher.write_usize(addr);
	}
}

cc_dyn!(
	#[derive(Clone, Debug)]
	CcObjectCore, ObjectCore,
	pub fn new() {...}
);

/// Either a flat Vec of cores or a nested LayeredCores reference.
#[derive(Trace, Debug)]
enum CoreSegment {
	Flat(Vec<CcObjectCore>),
	Nested(LayeredCores),
}
impl CoreSegment {
	fn len(&self) -> usize {
		match self {
			CoreSegment::Flat(v) => v.len(),
			CoreSegment::Nested(lc) => lc.len(),
		}
	}
}

/// A layered linked list of `CcObjectCore`s. Clone is O(1) via Cc.
/// `extend` creates a new node with `self` as parent — O(new_cores.len()).
/// `extend_layered` creates a new node with a nested LayeredCores — O(1).
/// Iteration walks the chain (parent first, then current).
/// This avoids the quadratic core-list copying that plain `Vec` extension
/// causes on deep mixin chains (rustanka coredump/OOM fix).
#[derive(Trace, Debug)]
#[trace(tracking(force))]
pub(super) struct LayeredCoresInner {
	parent: Option<LayeredCores>,
	parent_len: usize,
	current: CoreSegment,
}

#[derive(Trace, Debug)]
pub(super) struct LayeredCores(Cc<LayeredCoresInner>);
impl Clone for LayeredCores {
	fn clone(&self) -> Self {
		Self(self.0.clone())
	}
}
impl LayeredCores {
	fn new(cores: Vec<CcObjectCore>) -> Self {
		Self(Cc::new(LayeredCoresInner {
			parent: None,
			parent_len: 0,
			current: CoreSegment::Flat(cores),
		}))
	}
	fn empty() -> Self {
		Self::new(vec![])
	}
	fn extend(self, mut more: Vec<CcObjectCore>) -> Self {
		more.shrink_to_fit();
		let parent_len = self.len();
		Self(Cc::new(LayeredCoresInner {
			parent: Some(self),
			parent_len,
			current: CoreSegment::Flat(more),
		}))
	}
	/// Create a new layer with a nested LayeredCores as the current segment — O(1).
	fn extend_layered(self, more: LayeredCores) -> Self {
		if more.is_empty() {
			return self;
		}
		let parent_len = self.len();
		Self(Cc::new(LayeredCoresInner {
			parent: Some(self),
			parent_len,
			current: CoreSegment::Nested(more),
		}))
	}
	fn len(&self) -> usize {
		self.0.parent_len + self.0.current.len()
	}
	fn is_empty(&self) -> bool {
		self.len() == 0
	}
	/// Iterate all cores in forward order with absolute index, calling f for each.
	fn for_each_enumerated(&self, f: &mut dyn FnMut(usize, &CcObjectCore)) {
		if let Some(parent) = &self.0.parent {
			parent.for_each_enumerated(f);
		}
		let offset = self.0.parent_len;
		match &self.0.current {
			CoreSegment::Flat(v) => {
				for (i, core) in v.iter().enumerate() {
					f(offset + i, core);
				}
			}
			CoreSegment::Nested(lc) => {
				lc.for_each_enumerated(&mut |idx, core| f(offset + idx, core));
			}
		}
	}
	/// Iterate cores [0..limit) in reverse order with absolute index.
	/// Returns `Break` if the callback broke early, `Continue` otherwise.
	fn for_each_rev_up_to(
		&self,
		limit: usize,
		f: &mut dyn FnMut(usize, &CcObjectCore) -> ControlFlow<()>,
	) -> ControlFlow<()> {
		let offset = self.0.parent_len;
		match &self.0.current {
			CoreSegment::Flat(v) => {
				let end = limit.saturating_sub(offset).min(v.len());
				for i in (0..end).rev() {
					f(offset + i, &v[i])?;
				}
			}
			CoreSegment::Nested(lc) => {
				let nested_limit = limit.saturating_sub(offset).min(lc.len());
				if nested_limit > 0 {
					lc.for_each_rev_up_to(nested_limit, &mut |idx, core| f(offset + idx, core))?;
				}
			}
		}
		if let Some(parent) = &self.0.parent {
			parent.for_each_rev_up_to(limit.min(offset), f)?;
		}
		ControlFlow::Continue(())
	}
	/// Like for_each_rev_up_to but the callback can return Result.
	fn try_for_each_rev_up_to(
		&self,
		limit: usize,
		f: &mut dyn FnMut(usize, &CcObjectCore) -> Result<ControlFlow<()>, crate::error::Error>,
	) -> Result<ControlFlow<()>, crate::error::Error> {
		let offset = self.0.parent_len;
		match &self.0.current {
			CoreSegment::Flat(v) => {
				let end = limit.saturating_sub(offset).min(v.len());
				for i in (0..end).rev() {
					match f(offset + i, &v[i])? {
						ControlFlow::Continue(()) => {}
						ControlFlow::Break(()) => return Ok(ControlFlow::Break(())),
					}
				}
			}
			CoreSegment::Nested(lc) => {
				let nested_limit = limit.saturating_sub(offset).min(lc.len());
				if nested_limit > 0 {
					match lc.try_for_each_rev_up_to(nested_limit, &mut |idx, core| {
						f(offset + idx, core)
					})? {
						ControlFlow::Continue(()) => {}
						ControlFlow::Break(()) => return Ok(ControlFlow::Break(())),
					}
				}
			}
		}
		if let Some(parent) = &self.0.parent {
			return parent.try_for_each_rev_up_to(limit.min(offset), f);
		}
		Ok(ControlFlow::Continue(()))
	}
}

#[derive(Trace, Educe)]
#[educe(Debug)]
struct ObjValueInner {
	cores: LayeredCores,
	assertions_ran: Cell<bool>,
	has_assertions: bool,
	value_cache: RefCell<FxHashMap<(IStr, CoreIdx), CacheValue>>,
}

thread_local! {
	static RUNNING_ASSERTIONS: RefCell<FxHashSet<ObjValue>> = RefCell::default();
}
fn has_running_assertions() -> bool {
	RUNNING_ASSERTIONS.with_borrow(|v| !v.is_empty())
}
/// Returns false if already asserting
fn start_asserting(obj: &ObjValue) -> bool {
	RUNNING_ASSERTIONS.with_borrow_mut(|v| v.insert(obj.clone()))
}
// == Rustanka custom features ==

// Feature 1: SKIP_ASSERTIONS - skip all assertions during manifest generation (Go-Tanka compat)
thread_local! {
	static SKIP_ASSERTIONS: Cell<bool> = const { Cell::new(false) };
}
pub fn set_skip_assertions(skip: bool) {
	SKIP_ASSERTIONS.with(|v| v.set(skip));
}
pub fn should_skip_assertions() -> bool {
	SKIP_ASSERTIONS.with(|v| v.get())
}

/// Resets all thread-local state in obj to defaults.
pub fn reset_obj_thread_locals() {
	RUNNING_ASSERTIONS.with_borrow_mut(|v| v.clear());
	SKIP_ASSERTIONS.with(|v| v.set(false));
}

fn finish_asserting(obj: &ObjValue) {
	RUNNING_ASSERTIONS.with_borrow_mut(|v| {
		let r = v.remove(obj);
		debug_assert!(
			r,
			"finish_asserting was called before start_asserting or twice"
		);
	});
}

thread_local! {
	static EMPTY_OBJ: ObjValue = ObjValue(Cc::new(ObjValueInner {
		cores: LayeredCores::empty(),
		assertions_ran: Cell::new(true),
		has_assertions: false,
		value_cache: RefCell::default(),
	}))
}

#[allow(clippy::module_name_repetitions)]
#[derive(Clone, Trace, Debug, Educe)]
#[educe(PartialEq, Hash, Eq)]
pub struct ObjValue(
	#[educe(PartialEq(method(Cc::ptr_eq)), Hash(method(identity_hash)))] Cc<ObjValueInner>,
);

impl ObjValue {
	pub fn empty() -> Self {
		EMPTY_OBJ.with(Clone::clone)
	}
	pub fn is_empty(&self) -> bool {
		self.0.cores.is_empty() || self.len() == 0
	}
}

#[derive(Trace, Debug)]
pub(crate) struct StandaloneSuperCore {
	sup: CoreIdx,
	this: ObjValue,
}
impl ObjectCore for StandaloneSuperCore {
	fn enum_fields_core(
		&self,
		super_depth: &mut SuperDepth,
		handler: &mut EnumFieldsHandler<'_>,
	) -> bool {
		self.this.enum_fields_idx(super_depth, handler, self.sup)
	}

	fn has_field_include_hidden_core(&self, name: IStr) -> HasFieldIncludeHidden {
		if self.this.has_field_include_hidden_idx(name, self.sup) {
			HasFieldIncludeHidden::Exists
		} else {
			HasFieldIncludeHidden::NotFound
		}
	}

	fn get_for_core(&self, key: IStr, _sup_this: SupThis, omit_only: bool) -> Result<GetFor> {
		if omit_only {
			return Ok(GetFor::NotFound);
		}
		let v = self.this.get_idx(key, self.sup)?;
		Ok(v.map_or(GetFor::NotFound, GetFor::Final))
	}

	fn field_visibility_core(&self, field: IStr) -> FieldVisibility {
		self.this
			.field_visibility_idx(field, self.sup)
			.map_or(FieldVisibility::NotFound, FieldVisibility::Found)
	}

	fn run_assertions_core(&self, _sup_this: SupThis) -> Result<()> {
		self.this.run_assertions()
	}
}

#[derive(Debug, Acyclic)]
struct OmitFieldsCore {
	omit: FxHashSet<IStr>,
	prev_layers: usize,
}
impl ObjectCore for OmitFieldsCore {
	fn enum_fields_core(
		&self,
		super_depth: &mut SuperDepth,
		handler: &mut EnumFieldsHandler<'_>,
	) -> bool {
		let mut fi = FieldIndex::default();
		for f in &self.omit {
			if handler(
				*super_depth,
				fi,
				f.clone(),
				EnumFields::Omit(Saturating(self.prev_layers)),
			) == ControlFlow::Break(())
			{
				return false;
			}
			fi = fi.next();
		}
		true
	}

	fn has_field_include_hidden_core(&self, name: IStr) -> HasFieldIncludeHidden {
		if self.omit.contains(&name) {
			return HasFieldIncludeHidden::Omit(Saturating(self.prev_layers));
		}
		HasFieldIncludeHidden::NotFound
	}

	fn get_for_core(&self, key: IStr, _sup_this: SupThis, _omit_only: bool) -> Result<GetFor> {
		if self.omit.contains(&key) {
			return Ok(GetFor::Omit(Saturating(self.prev_layers)));
		}
		Ok(GetFor::NotFound)
	}

	fn field_visibility_core(&self, field: IStr) -> FieldVisibility {
		if self.omit.contains(&field) {
			return FieldVisibility::Omit(Saturating(self.prev_layers));
		}
		FieldVisibility::NotFound
	}

	fn run_assertions_core(&self, _sup_this: SupThis) -> Result<()> {
		Ok(())
	}
}

#[derive(Hash, PartialEq, Eq, Trace, Clone, Copy, Debug)]
struct CoreIdx {
	idx: usize,
}
impl CoreIdx {
	fn super_exists(self) -> bool {
		self.idx != 0
	}
}
#[derive(Trace, Clone, PartialEq, Eq, Hash, Debug)]
pub struct SupThis {
	sup: CoreIdx,
	this: ObjValue,
}
impl SupThis {
	/// Create a `SupThis` for a freshly constructed object (no super).
	pub fn new(this: ObjValue) -> Self {
		Self {
			sup: CoreIdx {
				idx: this.0.cores.len(),
			},
			this,
		}
	}
	pub fn has_super(&self) -> bool {
		self.sup.super_exists()
	}
	/// Implementation of `"field" in super` operation,
	/// works faster than standalone super path.
	///
	/// In case of no `super` existence, returns false.
	pub fn field_in_super(&self, field: IStr) -> bool {
		self.this.has_field_include_hidden_idx(field, self.sup)
	}
	/// Implementation of `super.field` operation,
	/// works faster than standalone super path.
	///
	/// In case of no `super` existence, returns `NoSuperFound`
	pub fn get_super(&self, field: IStr) -> Result<Option<Val>> {
		if !self.sup.super_exists() {
			bail!(NoSuperFound);
		}
		self.this.get_idx(field, self.sup)
	}
	/// `super` with `self` overriden for top-level lookups.
	/// Exists when super appears outside of `super.field`/`"field" in super` expressions
	/// Exclusive to jrsonnet.
	///
	/// Returns None if no `super` found
	pub fn standalone_super(&self) -> Option<ObjValue> {
		if !self.sup.super_exists() {
			return None;
		}
		let mut out = ObjValue::builder();
		out.extend_with_core(StandaloneSuperCore {
			sup: self.sup,
			this: self.this.clone(),
		});
		Some(out.build())
	}
	pub fn this(&self) -> &ObjValue {
		&self.this
	}
	pub fn downgrade(self) -> WeakSupThis {
		WeakSupThis {
			sup: self.sup,
			this: self.this.downgrade(),
		}
	}
}
#[derive(Trace, PartialEq, Eq, Hash, Debug)]
pub struct WeakSupThis {
	sup: CoreIdx,
	this: WeakObjValue,
}
impl WeakSupThis {
	/// Returns `true` if the referenced object is still alive.
	pub fn is_alive(&self) -> bool {
		self.this.is_alive()
	}
}

impl ObjValue {
	pub fn builder() -> ObjValueBuilder {
		ObjValueBuilder::new()
	}
	pub fn builder_with_capacity(capacity: usize) -> ObjValueBuilder {
		ObjValueBuilder::with_capacity(capacity)
	}
	pub(crate) fn extend_with_raw_member(self, key: IStr, value: ObjMember) -> Self {
		let mut out = ObjValueBuilder::with_capacity(1);
		out.with_super(self);
		let mut member = out.field(key);
		if value.flags.add() {
			member = member.add();
		}
		if let Some(loc) = value.location {
			member = member.with_location(loc);
		}
		let _ = member
			.with_visibility(value.flags.visibility())
			.binding(value.invoke);
		out.build()
	}
	pub fn extend_field(&mut self, name: IStr) -> ObjMemberBuilder<ExtendBuilder<'_>> {
		ObjMemberBuilder::new(ExtendBuilder(self), name, FieldIndex::default())
	}

	pub fn extend(&mut self) -> ObjValueBuilder {
		let mut out = ObjValueBuilder::new();
		out.with_super(self.clone());
		out
	}

	#[must_use]
	pub fn extend_from(&self, sup: Self) -> Self {
		// Chain sup's cores with self's cores — O(1) via nested LayeredCores.
		let cores = sup.0.cores.clone().extend_layered(self.0.cores.clone());

		let has_assertions = sup.0.has_assertions || self.0.has_assertions;
		ObjValue(Cc::new(ObjValueInner {
			cores,
			value_cache: RefCell::default(),
			assertions_ran: Cell::new(!has_assertions),
			has_assertions,
		}))
	}
	// #[must_use]
	// pub fn with_this(&self, this: Self) -> Self {
	// 	self.0.with_this(self.clone(), this)
	// }
	/// Returns amount of visible object fields
	/// If object only contains hidden fields - may return zero.
	pub fn len(&self) -> usize {
		self.fields_visibility()
			.values()
			.filter(|d| d.visible())
			.count()
	}
	pub fn len32(&self) -> u32 {
		arridx(self.len())
	}
	/// For each field, calls callback.
	/// If callback returns false - ends iteration prematurely.
	///
	/// Returns false if ended prematurely
	pub fn enum_fields(&self, handler: &mut EnumFieldsHandler<'_>) -> bool {
		let mut super_depth = SuperDepth::default();
		self.enum_fields_idx(
			&mut super_depth,
			handler,
			CoreIdx {
				idx: self.0.cores.len(),
			},
		)
	}

	fn enum_fields_idx(
		&self,
		super_depth: &mut SuperDepth,
		handler: &mut EnumFieldsHandler<'_>,
		idx: CoreIdx,
	) -> bool {
		let mut completed = true;
		let _ = self.0.cores.for_each_rev_up_to(idx.idx, &mut |_i, core| {
			if !core.0.enum_fields_core(super_depth, handler) {
				completed = false;
				return ControlFlow::Break(());
			}
			super_depth.deepen();
			ControlFlow::Continue(())
		});
		completed
	}

	pub fn has_field_include_hidden(&self, name: IStr) -> bool {
		self.has_field_include_hidden_idx(
			name,
			CoreIdx {
				idx: self.0.cores.len(),
			},
		)
	}
	fn has_field_include_hidden_idx(&self, name: IStr, core: CoreIdx) -> bool {
		let mut skip = Saturating(0usize);
		let mut found = false;
		let _ = self.0.cores.for_each_rev_up_to(core.idx, &mut |_i, ele| {
			match ele.0.has_field_include_hidden_core(name.clone()) {
				HasFieldIncludeHidden::Exists => {
					if skip.0 == 0 {
						found = true;
						return ControlFlow::Break(());
					}
				}
				HasFieldIncludeHidden::Omit(new_skip) => {
					// +1 including this core
					skip = skip.max(new_skip + Saturating(1));
				}
				HasFieldIncludeHidden::NotFound => {}
			}
			skip -= 1;
			ControlFlow::Continue(())
		});
		found
	}
	pub fn has_field(&self, name: IStr) -> bool {
		match self.field_visibility(name) {
			Some(Visibility::Unhide | Visibility::Normal) => true,
			Some(Visibility::Hidden) | None => false,
		}
	}
	pub fn has_field_ex(&self, name: IStr, include_hidden: bool) -> bool {
		if include_hidden {
			self.has_field_include_hidden(name)
		} else {
			self.has_field(name)
		}
	}
	pub fn get(&self, key: IStr) -> Result<Option<Val>> {
		self.get_idx(
			key,
			CoreIdx {
				idx: self.0.cores.len(),
			},
		)
	}

	fn get_idx(&self, key: IStr, core: CoreIdx) -> Result<Option<Val>> {
		let cache_key = (key.clone(), core);
		let assertion_reentry = {
			let mut cache = self.0.value_cache.borrow_mut();
			// entry_ref candidate?
			match cache.entry(cache_key.clone()) {
				Entry::Occupied(v) => match v.get() {
					CacheValue::Cached(v) => return v.clone(),
					CacheValue::Pending => {
						if !has_running_assertions() {
							bail!(InfiniteRecursionDetected);
						}
						true
					}
				},
				Entry::Vacant(v) => {
					v.insert(CacheValue::Pending);
					false
				}
			}
		};
		let result = self.get_idx_uncached(key, core, assertion_reentry);
		{
			let mut cache = self.0.value_cache.borrow_mut();
			cache.insert(cache_key, CacheValue::Cached(result.clone()));
		}
		result
	}
	fn get_idx_uncached(
		&self,
		key: IStr,
		core: CoreIdx,
		assertion_reentry: bool,
	) -> Result<Option<Val>> {
		if !assertion_reentry {
			self.run_assertions()?;
		}
		let mut first_add = None;
		let mut add_stack: Vec<Val> = Vec::new();
		let mut skip = Saturating(0);
		let mut early_return: Option<Val> = None;
		let _ = self
			.0
			.cores
			.try_for_each_rev_up_to(core.idx, &mut |sup, core| {
				let sup_this = SupThis {
					sup: CoreIdx { idx: sup },
					this: self.clone(),
				};
				match core.0.get_for_core(key.clone(), sup_this, skip.0 != 0)? {
					GetFor::Final(val) if first_add.is_none() => {
						if skip.0 == 0 {
							early_return = Some(val);
							return Ok(ControlFlow::Break(()));
						}
					}
					GetFor::Final(val) => {
						if skip.0 == 0 {
							add_stack.push(val);
							return Ok(ControlFlow::Break(()));
						}
					}
					GetFor::SuperPlus(val) => {
						if skip.0 == 0 {
							if first_add.is_none() {
								first_add = Some(val);
							} else {
								add_stack.push(val);
							}
						}
					}
					GetFor::Omit(new_skip) => {
						skip = skip.max(new_skip + Saturating(1));
					}
					GetFor::NotFound => {}
				}
				skip -= 1;
				Ok(ControlFlow::Continue(()))
			})?;
		if let Some(val) = early_return {
			return Ok(Some(val));
		}
		let Some(first) = first_add else {
			if add_stack.is_empty() {
				return Ok(None);
			}
			return Ok(Some(add_stack.pop().expect("single element on stack")));
		};
		if add_stack.is_empty() {
			return Ok(Some(first));
		}
		add_stack.insert(0, first);
		let mut values = add_stack.into_iter().rev();
		let init = values.next().expect("at least 2 elements");

		values
			.try_fold(init, |a, b| evaluate_add_op(&a, &b))
			.map(Some)
	}

	pub fn get_or_bail(&self, key: IStr) -> Result<Val> {
		let Some(value) = self.get(key.clone())? else {
			let suggestions = suggest_object_fields(self, key.clone());
			bail!(NoSuchField(key, suggestions))
		};
		Ok(value)
	}

	fn field_visibility(&self, field: IStr) -> Option<Visibility> {
		self.field_visibility_idx(
			field,
			CoreIdx {
				idx: self.0.cores.len(),
			},
		)
	}
	fn field_visibility_idx(&self, field: IStr, core: CoreIdx) -> Option<Visibility> {
		let mut exists = false;
		let mut skip = Saturating(0usize);
		let mut early_vis: Option<Visibility> = None;
		let _ = self.0.cores.for_each_rev_up_to(core.idx, &mut |_i, ele| {
			let vis = ele.0.field_visibility_core(field.clone());
			match vis {
				FieldVisibility::Found(vis @ (Visibility::Unhide | Visibility::Hidden)) => {
					if skip.0 == 0 {
						early_vis = Some(vis);
						return ControlFlow::Break(());
					}
				}
				FieldVisibility::Found(Visibility::Normal) => {
					if skip.0 == 0 {
						exists = true;
					}
				}
				FieldVisibility::NotFound => {}
				FieldVisibility::Omit(new_skip) => {
					// +1 including this core
					skip = skip.max(new_skip + Saturating(1));
				}
			}
			skip -= 1;
			ControlFlow::Continue(())
		});
		if early_vis.is_some() {
			return early_vis;
		}
		exists.then_some(Visibility::Normal)
	}

	pub fn run_assertions(&self) -> Result<()> {
		if should_skip_assertions() {
			return Ok(());
		}
		if self.0.assertions_ran.get() {
			return Ok(());
		}
		if !start_asserting(self) {
			return Ok(());
		}
		let mut assertion_err: Option<crate::error::Error> = None;
		self.0.cores.for_each_enumerated(&mut |idx, ele| {
			if assertion_err.is_some() {
				return;
			}
			let sup_this = SupThis {
				sup: CoreIdx { idx },
				this: self.clone(),
			};
			if let Err(e) = ele.0.run_assertions_core(sup_this) {
				assertion_err = Some(e);
			}
		});
		if let Some(e) = assertion_err {
			finish_asserting(self);
			return Err(e);
		}
		finish_asserting(self);
		self.0.assertions_ran.set(true);
		Ok(())
	}

	pub fn iter(
		&self,
		#[cfg(feature = "exp-preserve-order")] preserve_order: bool,
	) -> impl Iterator<Item = (IStr, Result<Val>)> + '_ {
		let fields = self.fields(
			#[cfg(feature = "exp-preserve-order")]
			preserve_order,
		);
		fields.into_iter().map(|field| {
			(
				field.clone(),
				self.get(field)
					.map(|opt| opt.expect("iterating over keys, field exists")),
			)
		})
	}
	pub fn get_lazy(&self, key: IStr) -> Option<Thunk<Val>> {
		#[derive(Trace)]
		struct ObjFieldThunk {
			obj: ObjValue,
			key: IStr,
		}
		impl ThunkValue for ObjFieldThunk {
			type Output = Val;

			fn get(&self) -> Result<Self::Output> {
				self.obj
					.get(self.key.clone())
					.transpose()
					.expect("field existence checked")
			}
		}

		if !self.has_field_ex(key.clone(), true) {
			return None;
		}

		Some(Thunk::new(ObjFieldThunk {
			obj: self.clone(),
			key,
		}))
	}
	pub fn get_lazy_or_bail(&self, key: IStr) -> Thunk<Val> {
		#[derive(Trace)]
		struct ObjFieldThunk {
			obj: ObjValue,
			key: IStr,
		}
		impl ThunkValue for ObjFieldThunk {
			type Output = Val;

			fn get(&self) -> Result<Self::Output> {
				self.obj.get_or_bail(self.key.clone())
			}
		}

		Thunk::new(ObjFieldThunk {
			obj: self.clone(),
			key,
		})
	}

	#[allow(dead_code, reason = "used in object ...rest destructuring")]
	pub(crate) fn as_standalone(&self) -> StandaloneSuperCore {
		StandaloneSuperCore {
			sup: CoreIdx {
				idx: self.0.cores.len(),
			},
			this: self.clone(),
		}
	}
	pub fn ptr_eq(a: &Self, b: &Self) -> bool {
		Cc::ptr_eq(&a.0, &b.0)
	}
	pub fn downgrade(self) -> WeakObjValue {
		WeakObjValue(self.0.downgrade())
	}
}

#[derive(Debug)]
struct FieldVisibilityData {
	omitted_until: Saturating<usize>,
	exists_visible: Option<Visibility>,
	#[allow(dead_code, reason = "used for exp-object-ordering, ZST otherwise")]
	key: FieldSortKey,
}
impl FieldVisibilityData {
	fn visible(&self) -> bool {
		self.exists_visible
			.expect("non-existing fields shall be dropped at the end of fn fields_visibility()")
			.is_visible()
	}
	#[allow(dead_code, reason = "used for exp-object-ordering, ZST otherwise")]
	fn sort_key(&self) -> FieldSortKey {
		self.key
	}
}

impl ObjValue {
	fn fields_visibility(&self) -> FxHashMap<IStr, FieldVisibilityData> {
		let mut out = FxHashMap::default();

		let mut super_depth = SuperDepth::default();
		let mut omit_index = Saturating(0);
		let _ = self
			.0
			.cores
			.for_each_rev_up_to(self.0.cores.len(), &mut |_i, core| {
				core.0
					.enum_fields_core(&mut super_depth, &mut |depth, index, name, visibility| {
						let entry = out.entry(name);
						let data = entry.or_insert_with(|| FieldVisibilityData {
							exists_visible: None,
							key: FieldSortKey::new(depth, index),
							omitted_until: omit_index,
						});
						match visibility {
							EnumFields::Omit(new_skip) => {
								// +1 including this core
								data.omitted_until = data
									.omitted_until
									.max(omit_index + new_skip + Saturating(1));
							}
							EnumFields::Normal(Visibility::Normal) => {
								if data.omitted_until <= omit_index && data.exists_visible.is_none()
								{
									data.exists_visible = Some(Visibility::Normal);
								}
							}
							EnumFields::Normal(Visibility::Hidden) => {
								if data.omitted_until <= omit_index {
									data.exists_visible = Some(match data.exists_visible {
										// We're iterating in reverse, later unhide is preserved
										Some(Visibility::Unhide) => Visibility::Unhide,
										_ => Visibility::Hidden,
									});
								}
							}
							EnumFields::Normal(Visibility::Unhide) => {
								if data.omitted_until <= omit_index {
									data.exists_visible = Some(match data.exists_visible {
										// We're iterating in reverse, later hide is preserved
										Some(Visibility::Hidden) => Visibility::Hidden,
										_ => Visibility::Unhide,
									});
								}
							}
						}
						ControlFlow::Continue(())
					});

				super_depth.deepen();
				omit_index += 1;
				ControlFlow::Continue(())
			});

		out.retain(|_, v| v.exists_visible.is_some());

		out
	}
	pub fn fields_with_visibility(
		&self,
		#[cfg(feature = "exp-preserve-order")] preserve_order: bool,
	) -> Vec<(IStr, Visibility)> {
		#[cfg(feature = "exp-preserve-order")]
		if preserve_order {
			let (mut fields, mut keys): (Vec<_>, Vec<_>) = self
				.fields_visibility()
				.into_iter()
				.enumerate()
				.map(|(idx, (k, d))| {
					(
						(
							k,
							d.exists_visible.expect("non-existing fields filtered out"),
						),
						(d.sort_key(), idx),
					)
				})
				.unzip();
			keys.sort_unstable_by_key(|v| v.0);
			for i in 0..fields.len() {
				let x = fields[i].clone();
				let mut j = i;
				loop {
					let k = keys[j].1;
					keys[j].1 = j;
					if k == i {
						break;
					}
					fields[j] = fields[k].clone();
					j = k;
				}
				fields[j] = x;
			}
			return fields;
		}
		let mut fields: Vec<_> = self
			.fields_visibility()
			.into_iter()
			.map(|(k, d)| {
				(
					k,
					d.exists_visible.expect("non-existing fields filtered out"),
				)
			})
			.collect();
		fields.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		fields
	}
	pub fn fields_ex(
		&self,
		include_hidden: bool,
		#[cfg(feature = "exp-preserve-order")] preserve_order: bool,
	) -> Vec<IStr> {
		#[cfg(feature = "exp-preserve-order")]
		if preserve_order {
			let (mut fields, mut keys): (Vec<_>, Vec<_>) = self
				.fields_visibility()
				.into_iter()
				.filter(|(_, d)| include_hidden || d.visible())
				.enumerate()
				.map(|(idx, (k, d))| (k, (d.sort_key(), idx)))
				.unzip();
			keys.sort_unstable_by_key(|v| v.0);
			// Reorder in-place by resulting indexes
			for i in 0..fields.len() {
				let x = fields[i].clone();
				let mut j = i;
				loop {
					let k = keys[j].1;
					keys[j].1 = j;
					if k == i {
						break;
					}
					fields[j] = fields[k].clone();
					j = k;
				}
				fields[j] = x;
			}
			return fields;
		}

		let mut fields: Vec<_> = self
			.fields_visibility()
			.into_iter()
			.filter(|(_, d)| include_hidden || d.visible())
			.map(|(k, _)| k)
			.collect();
		fields.sort_unstable();
		fields
	}
	pub fn fields(&self, #[cfg(feature = "exp-preserve-order")] preserve_order: bool) -> Vec<IStr> {
		self.fields_ex(
			false,
			#[cfg(feature = "exp-preserve-order")]
			preserve_order,
		)
	}
	pub fn values_ex(
		&self,
		include_hidden: bool,
		#[cfg(feature = "exp-preserve-order")] preserve_order: bool,
	) -> ArrValue {
		ArrValue::new(PickObjectValues::new(
			self.clone(),
			self.fields_ex(
				include_hidden,
				#[cfg(feature = "exp-preserve-order")]
				preserve_order,
			),
		))
	}
	pub fn values(&self, #[cfg(feature = "exp-preserve-order")] preserve_order: bool) -> ArrValue {
		self.values_ex(
			false,
			#[cfg(feature = "exp-preserve-order")]
			preserve_order,
		)
	}
	pub fn key_values_ex(
		&self,
		include_hidden: bool,
		#[cfg(feature = "exp-preserve-order")] preserve_order: bool,
	) -> ArrValue {
		ArrValue::new(PickObjectKeyValues::new(
			self.clone(),
			self.fields_ex(
				include_hidden,
				#[cfg(feature = "exp-preserve-order")]
				preserve_order,
			),
		))
	}
	pub fn key_values(
		&self,
		#[cfg(feature = "exp-preserve-order")] preserve_order: bool,
	) -> ArrValue {
		self.key_values_ex(
			false,
			#[cfg(feature = "exp-preserve-order")]
			preserve_order,
		)
	}
}

#[allow(clippy::module_name_repetitions)]
#[must_use = "value not added unless binding() was called"]
pub struct ObjMemberBuilder<Kind> {
	kind: Kind,
	name: IStr,
	add: bool,
	visibility: Visibility,
	original_index: FieldIndex,
	location: Option<Span>,
}

#[allow(clippy::missing_const_for_fn)]
impl<Kind> ObjMemberBuilder<Kind> {
	pub(crate) fn new(kind: Kind, name: IStr, original_index: FieldIndex) -> Self {
		Self {
			kind,
			name,
			original_index,
			add: false,
			visibility: Visibility::Normal,
			location: None,
		}
	}

	pub const fn with_add(mut self, add: bool) -> Self {
		self.add = add;
		self
	}
	pub fn add(self) -> Self {
		self.with_add(true)
	}
	pub fn with_visibility(mut self, visibility: Visibility) -> Self {
		self.visibility = visibility;
		self
	}
	pub fn hide(self) -> Self {
		self.with_visibility(Visibility::Hidden)
	}
	pub fn with_location(mut self, location: Span) -> Self {
		self.location = Some(location);
		self
	}
	fn build_member(self, binding: MaybeUnbound) -> (Kind, IStr, FieldIndex, ObjMember) {
		(
			self.kind,
			self.name,
			self.original_index,
			ObjMember {
				flags: ObjFieldFlags::new(self.add, self.visibility),
				invoke: binding,
				location: self.location,
			},
		)
	}
}

pub struct ExtendBuilder<'v>(&'v mut ObjValue);
impl ObjMemberBuilder<ExtendBuilder<'_>> {
	pub fn value(self, value: impl Into<Val>) {
		self.binding(MaybeUnbound::Const(value.into()));
	}
	pub fn bindable(self, bindable: impl Unbound<Bound = Val>) {
		self.binding(MaybeUnbound::Unbound(CcUnbound::new(bindable)));
	}
	pub fn binding(self, binding: MaybeUnbound) {
		let (receiver, name, _, member) = self.build_member(binding);
		let new = receiver.0.clone();
		*receiver.0 = new.extend_with_raw_member(name, member);
	}
}
