use std::alloc::{GlobalAlloc, Layout};
use std::ptr::NonNull;

use allocator_api2::alloc::{AllocError, Allocator};
use mimallocator::Mimalloc;

pub struct NotSoGlobalAllocator<A: GlobalAlloc>(pub A);

impl NotSoGlobalAllocator<Mimalloc> {
	pub fn get() -> &'static NotSoGlobalAllocator<Mimalloc> {
		static NOT_SO_GLOBAL_ALLOCATOR: NotSoGlobalAllocator<Mimalloc> =
			NotSoGlobalAllocator(Mimalloc);
		&NOT_SO_GLOBAL_ALLOCATOR
	}
}

/// A dangling, well-aligned pointer for zero-sized allocations.
///
/// `GlobalAlloc` does not support zero-sized layouts, but the `Allocator`
/// contract requires them: `allocate` must succeed without an underlying
/// allocation and `deallocate` of such a pointer must be a no-op (this is
/// what `alloc::Global` does).
#[inline]
fn zero_sized_dangling(layout: Layout) -> NonNull<[u8]> {
	// SAFETY: an alignment is always non-zero.
	let ptr = unsafe { NonNull::new_unchecked(layout.align() as *mut u8) };
	NonNull::<[u8]>::slice_from_raw_parts(ptr, 0)
}

unsafe impl<A> Allocator for NotSoGlobalAllocator<A>
where
	A: GlobalAlloc,
{
	#[inline]
	fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
		if layout.size() == 0 {
			return Ok(zero_sized_dangling(layout));
		}
		match NonNull::new(unsafe { self.0.alloc(layout) }) {
			Some(ptr) => Ok(NonNull::<[u8]>::slice_from_raw_parts(ptr, layout.size())),
			None => Err(AllocError),
		}
	}

	#[inline]
	fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
		if layout.size() == 0 {
			return Ok(zero_sized_dangling(layout));
		}
		match NonNull::new(unsafe { self.0.alloc_zeroed(layout) }) {
			Some(ptr) => Ok(NonNull::<[u8]>::slice_from_raw_parts(ptr, layout.size())),
			None => Err(AllocError),
		}
	}

	#[inline]
	unsafe fn grow(
		&self,
		ptr: NonNull<u8>,
		old_layout: Layout,
		new_layout: Layout,
	) -> Result<NonNull<[u8]>, AllocError> {
		debug_assert_eq!(
			old_layout.align(),
			new_layout.align(),
			"GlobalAlloc::realloc cannot change alignment"
		);
		if old_layout.size() == 0 {
			// `ptr` is dangling, there is nothing to reallocate.
			return self.allocate(new_layout);
		}
		match NonNull::new(unsafe { self.0.realloc(ptr.as_ptr(), old_layout, new_layout.size()) }) {
			Some(ptr) => Ok(NonNull::<[u8]>::slice_from_raw_parts(
				ptr,
				new_layout.size(),
			)),
			None => Err(AllocError),
		}
	}

	#[inline]
	unsafe fn grow_zeroed(
		&self,
		ptr: NonNull<u8>,
		old_layout: Layout,
		new_layout: Layout,
	) -> Result<NonNull<[u8]>, AllocError> {
		debug_assert_eq!(
			old_layout.align(),
			new_layout.align(),
			"GlobalAlloc::realloc cannot change alignment"
		);
		if old_layout.size() == 0 {
			// `ptr` is dangling, there is nothing to reallocate.
			return self.allocate_zeroed(new_layout);
		}
		let mut ptr = match NonNull::new(unsafe {
			self.0.realloc(ptr.as_ptr(), old_layout, new_layout.size())
		}) {
			Some(ptr) => Ok(NonNull::<[u8]>::slice_from_raw_parts(
				ptr,
				new_layout.size(),
			)),
			None => Err(AllocError),
		}?;
		unsafe { ptr.as_mut()[old_layout.size()..].fill(0) }
		Ok(ptr)
	}

	#[inline]
	unsafe fn shrink(
		&self,
		ptr: NonNull<u8>,
		old_layout: Layout,
		new_layout: Layout,
	) -> Result<NonNull<[u8]>, AllocError> {
		debug_assert_eq!(
			old_layout.align(),
			new_layout.align(),
			"GlobalAlloc::realloc cannot change alignment"
		);
		if new_layout.size() == 0 {
			unsafe { self.deallocate(ptr, old_layout) };
			return Ok(zero_sized_dangling(new_layout));
		}
		if old_layout.size() == 0 {
			// `ptr` is dangling, there is nothing to reallocate.
			return self.allocate(new_layout);
		}
		match NonNull::new(unsafe { self.0.realloc(ptr.as_ptr(), old_layout, new_layout.size()) }) {
			Some(ptr) => Ok(NonNull::<[u8]>::slice_from_raw_parts(
				ptr,
				new_layout.size(),
			)),
			None => Err(AllocError),
		}
	}

	#[inline]
	unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
		if layout.size() == 0 {
			// Zero-sized allocations are dangling pointers that were never
			// handed to the underlying `GlobalAlloc`.
			return;
		}
		unsafe {
			self.0.dealloc(ptr.as_ptr(), layout);
		}
	}
}
