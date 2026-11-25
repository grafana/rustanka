use std::alloc::{Allocator, Layout};
use std::rc::Rc;

use rtk_arena_allocator::{Arena, SharedArenaAlloc};

#[test]
fn container_keeps_heap_alive_and_drops_its_elements() {
	let alloc = SharedArenaAlloc::new();
	let probe = alloc.probe();
	assert_eq!(probe.heap_initialized(), Some(false));
	let element = Rc::new(42);
	let values = Box::new_in(element.clone(), alloc.clone());
	drop(alloc);
	assert_eq!(probe.heap_initialized(), Some(true));
	assert_eq!(**values, 42);
	drop(values);
	assert_eq!(Rc::strong_count(&element), 1);
	assert_eq!(probe.heap_initialized(), None);
}

#[test]
fn clones_grow_and_free_each_others_blocks() {
	let a = SharedArenaAlloc::new();
	let b = a.clone();
	let small = Layout::new::<[u8; 8]>();
	let large = Layout::new::<[u8; 128]>();
	let block = a.allocate(small).unwrap().cast::<[u8; 8]>();
	// SAFETY: both handles share an arena. Grow consumes the original block;
	// only its initialized prefix is read through the returned pointer.
	unsafe {
		block.write(*b"original");
		let grown = b.grow(block.cast(), small, large).unwrap();
		assert_eq!(grown.cast::<[u8; 8]>().read(), *b"original");
		a.deallocate(grown.cast(), large);
	}
}

#[test]
fn aligned_zero_size_allocation_does_not_initialize_heap() {
	let alloc = SharedArenaAlloc::new();
	let probe = alloc.probe();
	let layout = Layout::from_size_align(0, 8192).unwrap();
	let block = alloc.allocate(layout).unwrap();
	assert_eq!(block.len(), 0);
	assert_eq!(block.cast::<u8>().addr().get() % layout.align(), 0);
	assert_eq!(probe.heap_initialized(), Some(false));
	// SAFETY: this is the live block and layout returned by alloc.
	unsafe { alloc.deallocate(block.cast(), layout) };
	assert_eq!(probe.heap_initialized(), Some(false));
}

#[test]
fn borrowed_arena_supports_overaligned_storage() {
	#[repr(align(8192))]
	struct Item(u8);

	let arena = Arena::new();
	let mut items = Vec::with_capacity_in(1, arena.allocator());
	for n in 0..12 {
		items.push(Item(n));
	}
	assert_eq!(items.as_ptr().addr() % align_of::<Item>(), 0);
	assert!(items.iter().map(|item| item.0).eq(0..12));
}

#[test]
fn failed_allocation_leaves_heap_usable() {
	let alloc = SharedArenaAlloc::new();
	let first = Box::new_in(13u8, alloc.clone());
	let huge = Layout::from_size_align(isize::MAX as usize, 1).unwrap();
	assert!(alloc.allocate(huge).is_err());
	let second = Box::new_in(29u8, alloc);
	assert_eq!((*first, *second), (13, 29));
}

#[test]
fn destroying_heap_with_outstanding_raw_blocks() {
	let alloc = SharedArenaAlloc::new();
	let probe = alloc.probe();
	// Deliberately leave both blocks allocated. Raw blocks aren't owners;
	// only the weak probe may be consulted after the final handle drops.
	let _small = alloc.allocate(Layout::new::<u64>()).unwrap();
	let _large = alloc.allocate(Layout::new::<[u8; 32768]>()).unwrap();
	assert_eq!(probe.heap_initialized(), Some(true));
	drop(alloc);
	assert_eq!(probe.heap_initialized(), None);
}
