use crate::{Cc, Trace, Tracer, Weak};
use std::alloc::{Allocator, Global};
use std::ops::Deref;

/// Allocator handle whose state does not introduce collector edges.
///
/// # Safety
/// Neither the handle nor state it owns (including shared state reachable via
/// its clones) may own strong GC edges, directly or indirectly. This applies
/// throughout its lifetime, including state reached during deallocation/drop.
/// Raw storage managed by the allocator is distinct from owning the objects
/// in it: each live payload is owned and traced by its own box.
pub unsafe trait ArenaAllocator: Allocator + Clone + 'static {}

// SAFETY: Global has no owned state.
unsafe impl ArenaAllocator for Global {}

struct Payload<T: ?Sized, A: ArenaAllocator>(Box<T, A>);

// Conservatively track even sized payloads; T can also be a trait object.
impl<T: Trace + ?Sized, A: ArenaAllocator> Trace for Payload<T, A> {
	fn trace(&self, tracer: &mut Tracer) {
		T::trace(&self.0, tracer);
	}
}

/// Ambient cycle-collected pointer. Only the payload is allocated in `A`;
/// the ordinary global `Cc` control block owns its box and allocator handle.
pub struct ArenaCc<T: Trace + ?Sized, A: ArenaAllocator>(Cc<Payload<T, A>>);

/// Non-owning reference to an [`ArenaCc`]. Does not keep the allocator alive.
pub struct ArenaWeak<T: Trace + ?Sized, A: ArenaAllocator>(Weak<Payload<T, A>>);

impl<T: Trace, A: ArenaAllocator> ArenaCc<T, A> {
	/// Allocate `value` using `allocator` and register it with the thread collector.
	pub fn new_in(value: T, allocator: A) -> Self {
		Self::from_box(Box::new_in(value, allocator))
	}
}

impl<T: Trace + ?Sized, A: ArenaAllocator> ArenaCc<T, A> {
	/// Adopt an existing box without moving its payload. For a trait object,
	/// coerce the box to `Box<dyn Trait, A>` before passing it here.
	pub fn from_box(value: Box<T, A>) -> Self {
		Self(Cc::new(Payload(value)))
	}

	/// Create a weak reference to this allocation.
	pub fn downgrade(&self) -> ArenaWeak<T, A> {
		ArenaWeak(self.0.downgrade())
	}

	/// Compare control-block identity, not payload equality.
	pub fn ptr_eq(this: &Self, other: &Self) -> bool {
		Cc::ptr_eq(&this.0, &other.0)
	}
}

impl<T: Trace + ?Sized, A: ArenaAllocator> Clone for ArenaCc<T, A> {
	fn clone(&self) -> Self {
		Self(self.0.clone())
	}
}

impl<T: Trace + ?Sized, A: ArenaAllocator> Deref for ArenaCc<T, A> {
	type Target = T;
	fn deref(&self) -> &T {
		&self.0.deref().0
	}
}

impl<T: Trace + ?Sized, A: ArenaAllocator> Trace for ArenaCc<T, A> {
	fn trace(&self, tracer: &mut Tracer) {
		Cc::trace(&self.0, tracer);
	}
}

impl<T: Trace + ?Sized, A: ArenaAllocator> ArenaWeak<T, A> {
	/// Recover a strong reference if the payload has not been dropped.
	pub fn upgrade(&self) -> Option<ArenaCc<T, A>> {
		self.0.upgrade().map(ArenaCc)
	}

	/// Compare control-block identity, including after payload destruction.
	pub fn ptr_eq(this: &Self, other: &Self) -> bool {
		Weak::ptr_eq(&this.0, &other.0)
	}
}

impl<T: Trace + ?Sized, A: ArenaAllocator> Clone for ArenaWeak<T, A> {
	fn clone(&self) -> Self {
		Self(self.0.clone())
	}
}

impl<T: Trace + ?Sized, A: ArenaAllocator> Trace for ArenaWeak<T, A> {
	fn is_type_tracked() -> bool {
		false
	}
}
// SAFETY: weak references do not keep any payload alive.
unsafe impl<T: Trace + ?Sized, A: ArenaAllocator> crate::Acyclic for ArenaWeak<T, A> {}

#[cfg(test)]
mod tests {
	use super::*;
	use std::alloc::{AllocError, Layout};
	use std::cell::{Cell, RefCell};
	use std::ptr::NonNull;
	use std::rc::Rc;

	#[derive(Clone, Default)]
	struct Counting(Rc<Cell<usize>>);
	// SAFETY: the only shared state is a count of live allocations.
	unsafe impl ArenaAllocator for Counting {}
	// SAFETY: all allocations and matching deallocations go through Global.
	unsafe impl Allocator for Counting {
		fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
			let block = Global.allocate(layout)?;
			self.0.set(self.0.get() + 1);
			Ok(block)
		}
		unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
			self.0.set(self.0.get() - 1);
			unsafe { Global.deallocate(ptr, layout) }
		}
	}

	#[test]
	fn weak_does_not_retain_allocator() {
		let alloc = Counting::default();
		let value = ArenaCc::new_in(42, alloc.clone());
		let weak = value.downgrade();
		let other = weak.clone();
		assert_eq!(alloc.0.get(), 1);
		assert!(ArenaCc::ptr_eq(&value, &weak.upgrade().unwrap()));
		drop(value);
		assert_eq!(alloc.0.get(), 0);
		assert_eq!(Rc::strong_count(&alloc.0), 1);
		assert!(weak.upgrade().is_none());
		assert!(ArenaWeak::ptr_eq(&weak, &other));
	}

	trait ThunkValue: Trace {
		type Output;
		fn get(&self) -> Self::Output;
	}
	type Thunk = ArenaCc<dyn ThunkValue<Output = usize>, Counting>;
	struct Node {
		back: Cc<RefCell<Option<Thunk>>>,
		drops: Rc<Cell<usize>>,
	}
	impl Trace for Node {
		fn trace(&self, tracer: &mut Tracer) {
			self.back.trace(tracer);
		}
	}
	impl ThunkValue for Node {
		type Output = usize;
		fn get(&self) -> usize {
			self.drops.get()
		}
	}
	impl Drop for Node {
		fn drop(&mut self) {
			self.drops.set(self.drops.get() + 1);
		}
	}

	#[test]
	fn collect_cycle_through_dyn_payload_and_ordinary_cc() {
		let alloc = Counting::default();
		let drops = Rc::new(Cell::new(0));
		let back = Cc::new(RefCell::new(None));
		let boxed: Box<dyn ThunkValue<Output = usize>, Counting> = Box::new_in(
			Node {
				back: back.clone(),
				drops: drops.clone(),
			},
			alloc.clone(),
		);
		let thunk = Thunk::from_box(boxed);
		let weak = thunk.downgrade();
		*back.borrow_mut() = Some(thunk.clone());
		drop(back);
		assert_eq!(crate::collect_thread_cycles(), 0);
		assert_eq!(thunk.get(), 0);
		drop(thunk);
		assert_eq!(crate::collect_thread_cycles(), 2);
		assert!(weak.upgrade().is_none());
		assert_eq!(alloc.0.get(), 0);
		assert_eq!(Rc::strong_count(&alloc.0), 1);
		drop(weak);
		assert_eq!(drops.get(), 1);
	}
}
