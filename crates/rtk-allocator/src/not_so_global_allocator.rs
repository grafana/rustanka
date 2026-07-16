use std::alloc::{ Layout, GlobalAlloc };
use std::ptr::NonNull;

use allocator_api2::alloc::{Allocator, AllocError};
use mimallocator::Mimalloc;

pub struct NotSoGlobalAllocator<A: GlobalAlloc>(pub A);

impl NotSoGlobalAllocator<Mimalloc> {
    pub fn get() -> &'static NotSoGlobalAllocator<Mimalloc> {
        static NOT_SO_GLOBAL_ALLOCATOR: NotSoGlobalAllocator<Mimalloc> = NotSoGlobalAllocator(Mimalloc);
        &NOT_SO_GLOBAL_ALLOCATOR
    }
}

unsafe impl<A> Allocator for NotSoGlobalAllocator<A>
where
    A: GlobalAlloc,
{
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match NonNull::new(unsafe { self.0.alloc(layout) }) {
            Some(ptr) => Ok(NonNull::<[u8]>::slice_from_raw_parts(ptr, layout.size())),
            None => Err(AllocError),
        }
    }

    #[inline]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
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
        match NonNull::new(unsafe { self.0.realloc(ptr.as_ptr(), old_layout, new_layout.size()) }) {
            Some(ptr) => Ok(NonNull::<[u8]>::slice_from_raw_parts(ptr, new_layout.size())),
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
        let mut ptr = match NonNull::new(unsafe { self.0.realloc(ptr.as_ptr(), old_layout, new_layout.size()) }) {
            Some(ptr) => Ok(NonNull::<[u8]>::slice_from_raw_parts(ptr, new_layout.size())),
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
        match NonNull::new(unsafe { self.0.realloc(ptr.as_ptr(), old_layout, new_layout.size()) }) {
            Some(ptr) => Ok(NonNull::<[u8]>::slice_from_raw_parts(ptr, new_layout.size())),
            None => Err(AllocError),
        }
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe { self.0.dealloc(ptr.as_ptr(), layout); }
    }
}
