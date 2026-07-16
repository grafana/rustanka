#![allow(dead_code)]

#[cfg(not(debug_assertions))]
use std::cell::{BorrowError, BorrowMutError, UnsafeCell};
#[cfg(debug_assertions)]
use std::cell::{BorrowError, BorrowMutError, Ref, RefCell, RefMut};
use std::fmt::{self, Formatter};
use std::mem;
use std::ops::{Deref, DerefMut};

#[cfg(debug_assertions)]
#[derive(Debug)]
pub struct DebugRef<'a, T: ?Sized>(Ref<'a, T>);

#[cfg(not(debug_assertions))]
#[derive(Debug)]
pub struct DebugRef<'a, T: ?Sized>(&'a T);

impl<'a, T: ?Sized> Deref for DebugRef<'a, T> {
	type Target = T;

	#[inline]
	fn deref(&self) -> &Self::Target {
		self.0.deref()
	}
}

/// A `RefCell<T>` that only checks counts in `debug` mode.
#[cfg(debug_assertions)]
pub struct DebugRefCell<T: ?Sized>(RefCell<T>);

/// A `RefCell<T>` that only checks counts in `debug` mode.
#[cfg(not(debug_assertions))]
pub struct DebugRefCell<T: ?Sized>(UnsafeCell<T>);

#[cfg(debug_assertions)]
impl<T> DebugRefCell<T> {
	#[inline]
	pub const fn new(value: T) -> DebugRefCell<T> {
		DebugRefCell(RefCell::new(value))
	}
}

#[cfg(not(debug_assertions))]
impl<T> DebugRefCell<T> {
	#[inline]
	pub const fn new(value: T) -> DebugRefCell<T> {
		DebugRefCell(UnsafeCell::new(value))
	}
}

impl<T> DebugRefCell<T> {
	#[inline]
	pub fn into_inner(self) -> T {
		self.0.into_inner()
	}

	#[inline]
	pub fn replace(&self, t: T) -> T {
		mem::replace(&mut self.borrow_mut(), t)
	}

	#[inline]
	pub fn swap(&self, other: &DebugRefCell<T>) {
		mem::swap(&mut self.borrow_mut(), &mut other.borrow_mut())
	}
}

#[cfg(debug_assertions)]
impl<T: ?Sized> DebugRefCell<T> {
	#[inline]
	pub fn borrow<'a>(&'a self) -> DebugRef<'a, T> {
		DebugRef(self.0.borrow())
	}

	#[inline]
	pub fn borrow_mut<'a>(&'a self) -> DebugRefMut<'a, T> {
		DebugRefMut(self.0.borrow_mut())
	}

	#[inline]
	pub fn try_borrow<'a>(&'a self) -> Result<DebugRef<'a, T>, BorrowError> {
		self.0.try_borrow().map(DebugRef)
	}

	#[inline]
	pub fn try_borrow_mut<'a>(&'a self) -> Result<DebugRefMut<'a, T>, BorrowMutError> {
		self.0.try_borrow_mut().map(DebugRefMut)
	}
}

#[cfg(not(debug_assertions))]
impl<T: ?Sized> DebugRefCell<T> {
	#[inline]
	pub fn borrow<'a>(&'a self) -> DebugRef<'a, T> {
		unsafe { DebugRef(self.0.get().as_ref().expect("pointer will not be null")) }
	}

	#[inline]
	pub fn borrow_mut<'a>(&'a self) -> DebugRefMut<'a, T> {
		unsafe { DebugRefMut(self.0.get().as_mut().expect("pointer will not be null")) }
	}

	#[inline]
	pub fn try_borrow<'a>(&'a self) -> Result<DebugRef<'a, T>, BorrowError> {
		Ok(unsafe { DebugRef(self.0.get().as_ref().expect("pointer will not be null")) })
	}

	#[inline]
	pub fn try_borrow_mut<'a>(&'a self) -> Result<DebugRefMut<'a, T>, BorrowMutError> {
		Ok(unsafe { DebugRefMut(self.0.get().as_mut().expect("pointer will not be null")) })
	}
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for DebugRefCell<T> {
	#[inline]
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		fmt::Debug::fmt(&*self.borrow(), f)
	}
}

impl<T: Default> Default for DebugRefCell<T> {
	fn default() -> Self {
		DebugRefCell::new(T::default())
	}
}

#[cfg(debug_assertions)]
#[derive(Debug)]
pub struct DebugRefMut<'a, T: ?Sized>(RefMut<'a, T>);

#[cfg(not(debug_assertions))]
#[derive(Debug)]
pub struct DebugRefMut<'a, T: ?Sized>(&'a mut T);

impl<'a, T: ?Sized> Deref for DebugRefMut<'a, T> {
	type Target = T;

	#[inline]
	fn deref(&self) -> &Self::Target {
		self.0.deref()
	}
}

impl<'a, T: ?Sized> DerefMut for DebugRefMut<'a, T> {
	#[inline]
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.0.deref_mut()
	}
}
