//! Jrsonnet specific additional binding helpers

#[cfg(feature = "interop-common")]
mod common {
	use jrsonnet_evaluator::trace::{CompactFormat, HiDocFormat, JsFormat, PathResolver};

	use crate::VM;

	#[unsafe(no_mangle)]
	pub extern "C" fn jrsonnet_set_trace_format(vm: &mut VM, format: u8) {
		match format {
			0 => {
				vm.trace_format = Box::new(CompactFormat {
					max_trace: 20,
					resolver: PathResolver::new_cwd_fallback(),
					padding: 4,
				});
			}
			1 => vm.trace_format = Box::new(JsFormat { max_trace: 20 }),
			2 => {
				vm.trace_format = Box::new(HiDocFormat {
					resolver: PathResolver::new_cwd_fallback(),
					max_trace: 20,
				});
			}
			_ => panic!("unknown trace format"),
		}
	}
}

#[cfg(feature = "interop-threading")]
mod threading {
	use std::{ffi::c_int, thread::ThreadId};

	pub struct ThreadCTX {
		interner: *mut jrsonnet_interner::interop::PoolState,
		gc: *mut jrsonnet_gcmodule::interop::GcState,
	}

	/// Golang jrsonnet bindings require Jsonnet VM to be movable.
	/// Jrsonnet uses `thread_local` in some places, thus making VM
	/// immovable by default. By using `jrsonnet_exit_thread` and
	/// `jrsonnet_reenter_thread`, you can move `thread_local` state to
	/// where it is more convinient to use it.
	///
	/// # Safety
	///
	/// Current thread GC will be broken after this call, need to call
	/// `jrsonet_enter_thread` before doing anything.
	#[unsafe(no_mangle)]
	pub unsafe extern "C" fn jrsonnet_exit_thread() -> *mut ThreadCTX {
		Box::into_raw(Box::new(ThreadCTX {
			interner: jrsonnet_interner::interop::exit_thread(),
			gc: unsafe { jrsonnet_gcmodule::interop::exit_thread() },
		}))
	}

	#[unsafe(no_mangle)]
	pub extern "C" fn jrsonnet_reenter_thread(mut ctx: Box<ThreadCTX>) {
		use std::ptr::null_mut;
		assert!(
			!ctx.interner.is_null() && !ctx.gc.is_null(),
			"reused context?"
		);
		unsafe { jrsonnet_interner::interop::reenter_thread(ctx.interner) }
		unsafe { jrsonnet_gcmodule::interop::reenter_thread(ctx.gc) }
		// Just in case
		ctx.interner = null_mut();
		ctx.gc = null_mut();
	}

	// ThreadId is compatible with u64, and there is unstable cast
	// method... But until it is stabilized, lets erase its type by
	// boxing.
	pub enum JrThreadId {}

	#[unsafe(no_mangle)]
	pub extern "C" fn jrsonnet_thread_id() -> *mut JrThreadId {
		Box::into_raw(Box::new(std::thread::current().id())).cast()
	}

	#[unsafe(no_mangle)]
	pub extern "C" fn jrsonnet_thread_id_compare(
		a: *const JrThreadId,
		b: *const JrThreadId,
	) -> c_int {
		let a: &ThreadId = unsafe { *a.cast() };
		let b: &ThreadId = unsafe { *b.cast() };
		i32::from(*a == *b)
	}

	#[unsafe(no_mangle)]
	pub unsafe extern "C" fn jrsonnet_thread_id_free(id: *mut JrThreadId) {
		let _id: Box<ThreadId> = unsafe { Box::from_raw(id.cast()) };
	}
}
