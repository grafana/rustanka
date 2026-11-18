//! A single-threaded allocator backed by stable, individually owned pages.
//! Allocation prefers freed blocks of the same layout, then unused page tails.
//! If neither fits, it adds a pseudo-page rounded up to a multiple of 4 KiB
//! and aligned for the request. Empty pages reset for reuse by any layout;
//! pages are returned to the global allocator only when the arena is dropped.
//!
//! A container cannot escape the arena it borrows:
//! ```compile_fail,E0597
//! use rtk_arena_allocator::Arena;
//! let values = {
//!     let arena = Arena::new();
//!     Vec::<u8, _>::new_in(arena.allocator())
//! };
//! drop(values);
//! ```

use std::alloc::{AllocError, Allocator, Global, Layout};
use std::cell::RefCell;
use std::ptr::NonNull;

const PAGE_SIZE: usize = 4096;

/// Owns the pages shared by all its allocator handles.
#[derive(Default)]
pub struct Arena {
	pages: RefCell<Vec<Page>>,
}

impl Arena {
	/// Creates an empty arena. Storage is acquired on demand.
	pub fn new() -> Self {
		Self::default()
	}

	/// Acquires a page from the global allocator. Zero-sized pages are rejected.
	/// Existing blocks remain valid even if the page directory must grow.
	pub fn add_page(&self, layout: Layout) -> Result<(), AllocError> {
		if layout.size() == 0 {
			return Err(AllocError);
		}
		let mut pages = self.pages.borrow_mut();
		pages.try_reserve(1).map_err(|_| AllocError)?;
		pages.push(Page::new(layout)?);
		Ok(())
	}

	/// Borrows the arena without acquiring memory.
	pub fn allocator(&self) -> ArenaAlloc<'_> {
		ArenaAlloc(self)
	}
}

struct Page {
	base: NonNull<u8>,
	layout: Layout,
	used: usize,
	live: usize,
	free: Vec<(usize, Layout)>,
}

impl Page {
	fn new(layout: Layout) -> Result<Self, AllocError> {
		Ok(Self {
			base: Global.allocate(layout)?.cast(),
			layout,
			used: 0,
			live: 0,
			free: Vec::new(),
		})
	}

	fn reuse(&mut self, layout: Layout) -> Option<NonNull<[u8]>> {
		let index = self.free.iter().rposition(|&(_, old)| old == layout)?;
		let (start, _) = self.free.swap_remove(index);
		self.live += 1;
		// SAFETY: the removed entry described a non-live block within this page
		// with exactly this layout. Offsets retain the page base's provenance.
		Some(NonNull::slice_from_raw_parts(
			unsafe { self.base.add(start) },
			layout.size(),
		))
	}

	fn bump_start(&self, layout: Layout) -> Option<usize> {
		let cursor = self.base.as_ptr().wrapping_add(self.used);
		let start = self.used.checked_add(cursor.align_offset(layout.align()))?;
		let end = start.checked_add(layout.size())?;
		if end > self.layout.size() {
			return None;
		}
		Some(start)
	}

	fn bump(&mut self, layout: Layout) -> Result<Option<NonNull<[u8]>>, AllocError> {
		let Some(start) = self.bump_start(layout) else {
			return Ok(None);
		};
		// Keep capacity for every live block to be freed without allocating.
		self.free
			.try_reserve(self.live + 1)
			.map_err(|_| AllocError)?;
		self.used = start + layout.size();
		self.live += 1;
		// SAFETY: bump_start checked alignment and bounds beyond all prior blocks.
		Ok(Some(NonNull::slice_from_raw_parts(
			unsafe { self.base.add(start) },
			layout.size(),
		)))
	}
}

impl Drop for Page {
	fn drop(&mut self) {
		// SAFETY: the page uniquely owns this Global allocation and retains its layout.
		unsafe { Global.deallocate(self.base, self.layout) };
	}
}

/// A copyable handle; all copies can free blocks allocated by the same arena.
#[derive(Clone, Copy)]
pub struct ArenaAlloc<'a>(&'a Arena);

// SAFETY: handles borrow stable page storage. The shared directory serializes
// cursor updates; a page's cursor only rewinds when no nonempty blocks remain.
// Each allocation reports exactly its requested size, so a fitting layout has
// that same size. Zero-sized blocks never consume or release page space.
unsafe impl Allocator for ArenaAlloc<'_> {
	fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
		if layout.size() == 0 {
			return Ok(NonNull::slice_from_raw_parts(layout.dangling_ptr(), 0));
		}
		let mut pages = self.0.pages.borrow_mut();
		if let Some(block) = pages.iter_mut().find_map(|p| p.reuse(layout)) {
			return Ok(block);
		}
		for page in pages.iter_mut() {
			if let Some(block) = page.bump(layout)? {
				return Ok(block);
			}
		}
		let size = layout
			.size()
			.checked_next_multiple_of(PAGE_SIZE)
			.ok_or(AllocError)?;
		let backing = Layout::from_size_align(size, layout.align()).map_err(|_| AllocError)?;
		pages.try_reserve(1).map_err(|_| AllocError)?;
		let mut page = Page::new(backing)?;
		let block = page.bump(layout)?.ok_or(AllocError)?;
		pages.push(page);
		Ok(block)
	}

	unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
		if layout.size() == 0 {
			return;
		}
		let addr = ptr.addr().get();
		let mut pages = self.0.pages.borrow_mut();
		// The Allocator caller guarantees ptr is a live block from this arena.
		let page = pages
			.iter_mut()
			.find(|p| addr.wrapping_sub(p.base.addr().get()) < p.layout.size())
			.expect("block belongs to arena");
		page.live -= 1;
		if page.live == 0 {
			page.free.clear(); // No stale entries may survive the rewind.
			page.used = 0;
		} else {
			page.free.push((addr - page.base.addr().get(), layout));
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn explicit_pages_and_reuse() {
		let arena = Arena::new();
		let a = arena.allocator();

		arena.add_page(Layout::new::<[u64; 2]>()).unwrap();
		let first = Box::new_in(11_u64, a);
		let address = (&*first) as *const u64;
		let second = Box::new_in(22_u64, a);
		drop(first);
		let reused = Box::new_in(44_u64, arena.allocator());
		assert_eq!(address, &*reused as *const u64);
		assert_eq!(arena.pages.borrow().len(), 1);
		let third = Box::new_in(33_u64, a);
		assert_eq!(arena.pages.borrow().len(), 2);
		assert_eq!((*second, *reused, *third), (22, 44, 33));
		drop((second, reused));
		let whole = Box::new_in([7_u64; 2], a);
		assert_eq!(whole.as_ptr(), address);
	}

	#[test]
	fn failed_growth_leaves_original_block_live() {
		let arena = Arena::new();
		let a = arena.allocator();
		let mut values = Vec::with_capacity_in(1, a);
		values.push(42u8);
		let before = values.as_ptr();
		// A valid byte layout, but rounding it to a page exceeds isize::MAX.
		let huge = Layout::from_size_align(isize::MAX as usize, 1).unwrap();
		assert!(a.allocate(huge).is_err());
		assert!(values.try_reserve_exact(huge.size() - 1).is_err());
		assert_eq!(values.as_ptr(), before);
		assert_eq!(values.as_slice(), &[42]);
		assert_eq!(arena.pages.borrow().len(), 1);
	}

	#[test]
	fn aligned_zero_size_without_page() {
		let arena = Arena::new();
		let layout = Layout::from_size_align(0, 4096).unwrap();
		assert!(arena.add_page(layout).is_err());
		let a = arena.allocator();
		let p = a.allocate(layout).unwrap();
		assert_eq!(p.len(), 0);
		assert_eq!(p.cast::<u8>().addr().get() % layout.align(), 0);
		// SAFETY: same handle and layout as the allocation above.
		unsafe { a.deallocate(p.cast(), layout) };
	}

	#[test]
	fn vec_growth_preserves_overaligned_values() {
		#[repr(align(8192))]
		struct Item(u8);
		let arena = Arena::new();

		let _prefix = Box::new_in(1u8, arena.allocator());
		let mut values = Vec::new_in(arena.allocator());
		for n in 0..12 {
			values.push(Item(n));
		}
		values.truncate(7);
		values.shrink_to_fit();
		assert_eq!(values.capacity(), 7);
		assert_eq!(values.as_ptr().addr() % 8192, 0);
		assert!(values.iter().map(|v| v.0).eq(0..7));
	}
}
