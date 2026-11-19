//! Scope-end collection, independent of `Cc` and `Trace` (`scoped-arena` feature).
//!
//! Values live until the callback returns or unwinds; there is no tracing or early
//! collection. Edges are indices, not owning references. The registry drops its
//! boxes even if handles form cycles or are forgotten. Drop order is unspecified.
//! Destructors can open fresh scopes but cannot look up peers during teardown:
//! access requires the callback's borrow of the arena, which has already ended.
//! Ordinary Rust unwind rules apply (in particular a second panic can abort).
//!
//! ```
//! use jrsonnet_gcmodule::scoped::{scope, Handle};
//! use std::cell::Cell;
//! struct Node<'id> { name: &'id str, next: Cell<Option<Handle<'id, Node<'id>>>> }
//! let name = String::from("root");
//! scope(|arena| {
//!     let root = arena.alloc(Node { name: &name, next: Cell::new(None) });
//!     arena.get(root).next.set(Some(root));
//!     assert_eq!(arena.get(arena.get(root).next.get().unwrap()).name, "root");
//! });
//! ```
//!
//! Handles cannot escape, cross scopes, or change payload type:
//!
//! ```compile_fail
//! use jrsonnet_gcmodule::scoped::scope;
//! let escaped = scope(|a| a.alloc(1));
//! ```
//! ```compile_fail
//! use jrsonnet_gcmodule::scoped::scope;
//! scope(|a| { let h = a.alloc(1); scope(|b| { b.get(h); }); });
//! ```
//! ```compile_fail
//! use jrsonnet_gcmodule::scoped::scope;
//! scope(|a| { let h = a.alloc(1u32); let _: &f32 = a.get(h); });
//! ```
//! Payload lifetimes are invariant too: a handle cannot be relabeled even
//! when both types have the same layout.
//! ```compile_fail
//! use jrsonnet_gcmodule::scoped::Handle;
//! fn shorten<'id, 'short>(h: Handle<'id, &'static str>) -> Handle<'id, &'short str> { h }
//! ```
//! Callback-local borrows that would dangle at teardown are rejected:
//! ```compile_fail
//! use jrsonnet_gcmodule::scoped::scope;
//! scope(|a| { let local = String::new(); a.alloc(&local); });
//! ```

//! References to payloads cannot escape either:
//! ```compile_fail
//! use jrsonnet_gcmodule::scoped::scope;
//! let escaped = scope(|a| { let h = a.alloc(1); a.get(h) });
//! ```

use std::alloc::{Allocator, Global};
use std::marker::PhantomData;

type Invariant<T> = PhantomData<fn(T) -> T>;

/// A non-owning index. Only the matching arena can resolve it.
pub struct Handle<'id, T> {
	index: usize,
	marker: Invariant<(&'id (), T)>,
}

impl<T> Copy for Handle<'_, T> {}
impl<T> Clone for Handle<'_, T> {
	fn clone(&self) -> Self {
		*self
	}
}

// Unlike Any, this trait doesn't require 'static. The box retains its concrete
// drop glue and allocator even if no handle to its payload survives.
trait Slot {}
impl<T> Slot for T {}

/// The unique registry for a fresh invariant brand. Allocation exclusively
/// borrows it; use interior mutability for edges constructed after allocation.
pub struct Arena<'id, 'env: 'id, A: Allocator> {
	slots: Vec<Box<dyn Slot + 'id, &'env A>, &'env A>,
	brand: Invariant<&'id ()>,
}

/// Run a fresh scope using the global allocator.
pub fn scope<'env, R>(f: impl for<'id> FnOnce(&mut Arena<'id, 'env, Global>) -> R) -> R {
	scope_in(&Global, f)
}

/// Run a fresh scope using `alloc` for both registry storage and payload boxes.
/// Allocations made by payloads themselves are not redirected.
pub fn scope_in<'env, A: Allocator, R>(
	alloc: &'env A,
	f: impl for<'id> FnOnce(&mut Arena<'id, 'env, A>) -> R,
) -> R {
	f(&mut Arena {
		slots: Vec::new_in(alloc),
		brand: PhantomData,
	})
}

impl<'id, 'env: 'id, A: Allocator> Arena<'id, 'env, A> {
	/// Register a payload, which may borrow data outliving the scope.
	pub fn alloc<T: 'id>(&mut self, value: T) -> Handle<'id, T> {
		let index = self.slots.len();
		self.slots.push(Box::new_in(value, *self.slots.allocator()));
		Handle {
			index,
			marker: PhantomData,
		}
	}

	/// Borrow a payload for the duration of a borrow of this registry.
	pub fn get<T>(&self, handle: Handle<'id, T>) -> &T {
		let slot: &dyn Slot = &*self.slots[handle.index];
		// SAFETY: alloc alone issues handles, only after inserting Box<T> at
		// index. Slots are never replaced or removed. scope_in lends exactly
		// one arena per higher-ranked brand; both arena and handle are invariant
		// in that brand, and the handle is invariant in T (including lifetimes).
		// Casting the trait object pointer discards metadata, retaining the
		// original T data pointer. Its box stays shared-borrowed through self.
		unsafe { &*std::ptr::from_ref(slot).cast::<T>() }
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::alloc::{AllocError, Layout};
	use std::cell::Cell;
	use std::panic::{AssertUnwindSafe, catch_unwind};
	use std::ptr::NonNull;

	struct OnDrop<F: FnOnce()>(Option<F>);
	impl<F: FnOnce()> Drop for OnDrop<F> {
		fn drop(&mut self) {
			self.0.take().unwrap()();
		}
	}

	struct Counter(Cell<usize>);
	// SAFETY: all allocations and matching deallocations forward to Global.
	unsafe impl Allocator for Counter {
		fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
			let block = Global.allocate(layout)?;
			self.0.set(self.0.get() + 1);
			Ok(block)
		}
		unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
			self.0.set(self.0.get() - 1);
			// SAFETY: caller supplies the original Global allocation and layout.
			unsafe { Global.deallocate(ptr, layout) }
		}
	}

	#[test]
	fn cycle_and_forgotten_handles_do_not_own_storage() {
		struct Node<'id> {
			next: Cell<Option<Handle<'id, Node<'id>>>>,
			drops: &'id Cell<usize>,
		}
		impl Drop for Node<'_> {
			fn drop(&mut self) {
				self.drops.set(self.drops.get() + 1);
			}
		}
		let alloc = Counter(Cell::new(0));
		let drops = Cell::new(0);
		scope_in(&alloc, |a| {
			let left = a.alloc(Node {
				next: Cell::new(None),
				drops: &drops,
			});
			let right = a.alloc(Node {
				next: Cell::new(Some(left)),
				drops: &drops,
			});
			a.get(left).next.set(Some(right));
			for n in 0..128 {
				a.alloc(n);
			}
			assert!(std::ptr::eq(
				a.get(left),
				a.get(a.get(right).next.get().unwrap())
			));
			#[allow(forgetting_copy_types)] // A forgotten index has no cleanup duty.
			std::mem::forget(left);
			assert_eq!(drops.get(), 0);
			assert_eq!(alloc.0.get(), 131); // 130 non-ZST boxes plus registry
		});
		assert_eq!(alloc.0.get(), 0);
		assert_eq!(drops.get(), 2);
	}

	#[test]
	fn callback_unwind_and_drop_reentry() {
		let drops = Cell::new(0);
		let result = catch_unwind(AssertUnwindSafe(|| {
			scope(|outer| {
				outer.alloc(OnDrop(Some(|| {
					scope(|inner| {
						let h = inner.alloc(&drops);
						inner.get(h).set(1);
					})
				})));
				panic!("scope body");
			})
		}));
		assert!(result.is_err());
		assert_eq!(drops.get(), 1);
	}

	#[test]
	fn panicking_drop_still_releases_other_slots_and_storage() {
		let alloc = Counter(Cell::new(0));
		let calls = Cell::new(0);
		let result = catch_unwind(AssertUnwindSafe(|| {
			scope_in(&alloc, |a| {
				for i in 0..3 {
					a.alloc(OnDrop(Some(|| calls.set(calls.get() + 1))));
					if i == 1 {
						a.alloc(OnDrop(Some(|| panic!("payload"))));
					}
				}
			})
		}));
		assert!(result.is_err());
		assert_eq!(calls.get(), 3);
		assert_eq!(alloc.0.get(), 0);
	}
}
