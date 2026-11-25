//! A private mimalloc heap, created on the first nonzero allocation. Individual
//! blocks may be freed normally; dropping the owner frees all remaining blocks.
//! Heap destruction releases storage, not Rust values: containers still run their
//! own destructors. A raw pointer must not be used or freed after heap destruction.
//!
//! ```compile_fail,E0597
//! use rtk_arena_allocator::Arena;
//! let values = {
//!     let arena = Arena::new();
//!     Vec::<u8, _>::new_in(arena.allocator())
//! };
//! drop(values);
//! ```

use std::alloc::{AllocError, Allocator, Layout};
use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};

use libmimalloc_sys as mi;

/// Unique, thread-confined owner of a heap. Moving the owner doesn't move the heap.
#[derive(Default)]
pub struct Arena {
	heap: Cell<Option<NonNull<mi::mi_heap_t>>>,
}

impl Arena {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn allocator(&self) -> ArenaAlloc<'_> {
		self
	}

	fn heap(&self) -> Result<NonNull<mi::mi_heap_t>, AllocError> {
		if let Some(heap) = self.heap.get() {
			return Ok(heap);
		}
		// SAFETY: creates a fresh heap; it is never installed as a thread default.
		let heap = NonNull::new(unsafe { mi::mi_heap_new() }).ok_or(AllocError)?;
		self.heap.set(Some(heap));
		Ok(heap)
	}
}

impl Drop for Arena {
	fn drop(&mut self) {
		if let Some(heap) = self.heap.take() {
			// SAFETY: unique owner, no borrowed or shared allocator handle remains.
			// Unlike mi_heap_delete, destroy also frees outstanding raw allocations.
			unsafe { mi::mi_heap_destroy(heap.as_ptr()) };
		}
	}
}

/// A container using this handle cannot outlive the heap owner.
pub type ArenaAlloc<'a> = &'a Arena;

// SAFETY: the heap identity is stable across moves; nonzero blocks are obtained
// from mimalloc with their requested alignment and freed only through mimalloc.
unsafe impl Allocator for Arena {
	fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
		if layout.size() == 0 {
			return Ok(NonNull::slice_from_raw_parts(layout.dangling_ptr(), 0));
		}
		let heap = self.heap()?;
		// SAFETY: heap is live and Layout supplies a nonzero power-of-two alignment.
		let block =
			unsafe { mi::mi_heap_malloc_aligned(heap.as_ptr(), layout.size(), layout.align()) };
		let ptr = NonNull::new(block.cast::<u8>()).ok_or(AllocError)?;
		Ok(NonNull::slice_from_raw_parts(ptr, layout.size()))
	}

	unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
		if layout.size() != 0 {
			// SAFETY: caller supplies a live block from this allocator; mimalloc
			// determines its originating heap without consulting the active evaluation.
			unsafe { mi::mi_free(ptr.as_ptr().cast()) };
		}
	}
}

/// An owning allocator for lazy values that may outlive their originating scope.
/// Clones share one heap. Only the last strong owner destroys it.
#[derive(Clone, Default)]
pub struct SharedArenaAlloc(Rc<Arena>);

/// Inspect a heap's lifetime without extending it.
#[derive(Clone, Debug)]
pub struct ArenaProbe(Weak<Arena>);

impl ArenaProbe {
	/// `None` after destruction, otherwise whether the native heap has been created.
	pub fn heap_initialized(&self) -> Option<bool> {
		Some(self.0.upgrade()?.heap.get().is_some())
	}
}

impl SharedArenaAlloc {
	pub fn probe(&self) -> ArenaProbe {
		ArenaProbe(Rc::downgrade(&self.0))
	}

	pub fn new() -> Self {
		Self::default()
	}
}

impl From<Rc<Arena>> for SharedArenaAlloc {
	fn from(arena: Rc<Arena>) -> Self {
		Self(arena)
	}
}

// SAFETY: every clone keeps the same Arena alive and uses it for both operations.
unsafe impl Allocator for SharedArenaAlloc {
	fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
		self.0.allocate(layout)
	}

	unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
		// SAFETY: the caller guarantees the block belongs to this allocation domain.
		unsafe { self.0.deallocate(ptr, layout) };
	}
}
