#![allow(
	clippy::implicit_hasher,
	reason = "those methods exist exactly because with_capacity is only present for default BuildHasher"
)]

/// Macros to help deal with Gc
use jrsonnet_gcmodule::Trace;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};

pub trait WithCapacityExt {
	fn new() -> Self;
	fn with_capacity(capacity: usize) -> Self;
}
impl<V> WithCapacityExt for FxHashSet<V> {
	fn with_capacity(capacity: usize) -> Self {
		Self::with_capacity_and_hasher(capacity, FxBuildHasher)
	}

	fn new() -> Self {
		Self::with_hasher(FxBuildHasher)
	}
}
impl<K, V> WithCapacityExt for FxHashMap<K, V> {
	fn with_capacity(capacity: usize) -> Self {
		Self::with_capacity_and_hasher(capacity, FxBuildHasher)
	}

	fn new() -> Self {
		Self::with_hasher(FxBuildHasher)
	}
}

pub fn assert_trace<T: Trace>(_v: &T) {}

#[cfg(feature = "scoped-arena")]
pub(crate) mod payload {
	use std::alloc::{AllocError, Allocator, Global, Layout};
	use std::ptr::NonNull;

	use jrsonnet_gcmodule::ArenaAllocator;
	use rtk_arena_allocator::SharedArenaAlloc;

	// Keep the allocator choice in the payload box, not in every evaluator handle.
	#[derive(Clone)]
	pub(crate) struct PayloadAlloc(pub(crate) Option<SharedArenaAlloc>);

	impl PayloadAlloc {
		pub(crate) fn current() -> Self {
			crate::with_lazy_allocator(|alloc| Self(alloc.cloned()))
		}
	}

	// SAFETY: clones preserve the selected allocation domain, including after state exit.
	unsafe impl Allocator for PayloadAlloc {
		fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
			match &self.0 {
				Some(a) => a.allocate(layout),
				None => Global.allocate(layout),
			}
		}

		unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
			// SAFETY: the box supplies its original allocator and matching block/layout.
			unsafe {
				match &self.0 {
					Some(a) => a.deallocate(ptr, layout),
					None => Global.deallocate(ptr, layout),
				}
			}
		}
	}

	// SAFETY: SharedArenaAlloc owns raw pages and bookkeeping, not the boxed
	// values in those pages or any other collector edges. Global is stateless.
	unsafe impl ArenaAllocator for PayloadAlloc {}
}
