//! Runtime bindings to the reference C++ Jsonnet interpreter's published C API.
//!
//! The shared library is opened when [`Implementation::new`] is called, never at
//! build time. Its public API returns manifested JSON, rather than the live
//! thunks expected by some callers. The core interface here is eager: evaluating
//! an object forces every visible field before returning to Rust.

use std::ffi::{CStr, CString, OsStr, c_char, c_int, c_uint, c_void};
use std::fmt;
use std::marker::PhantomData;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;

mod core;
mod native;
mod serde;

pub use core::{Evaluator, EvaluatorError, Flag};
pub use native::{Arguments, Array, Object, Value};
pub use serde::{ValueDeserializer, ValueSerializer};

use libloading::Library;
use thiserror::Error;

/// The C ABI is checked against this upstream version before other symbols are used.
const VERSION: &str = "v0.22.0";

#[cfg(target_os = "linux")]
const DEFAULT_LIBRARY: &str = "libjsonnet.so.0";
#[cfg(target_os = "macos")]
const DEFAULT_LIBRARY: &str = "libjsonnet.dylib";
#[cfg(target_os = "windows")]
const DEFAULT_LIBRARY: &str = "jsonnet.dll";
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const DEFAULT_LIBRARY: &str = "libjsonnet.so";

/// The reference C API forces native arguments before calling this function.
/// Its arguments are JSON values, not live Jsonnet thunks. The callback must
/// never unwind across the C ABI; use `success = 0` and a string value on error.
pub type NativeCallback =
	unsafe extern "C" fn(*mut c_void, *const *const c_void, *mut c_int) -> *mut c_void;

#[derive(Debug, Error)]
pub enum Error {
	#[error("loading reference Jsonnet library {path}: {source}")]
	Load {
		path: String,
		#[source]
		source: libloading::Error,
	},
	#[error("reference Jsonnet library is missing {symbol}: {source}")]
	Symbol {
		symbol: &'static str,
		#[source]
		source: libloading::Error,
	},
	#[error("reference Jsonnet library version mismatch: expected {VERSION}, found {found}")]
	Version { found: String },
	#[error("reference Jsonnet returned a null pointer for {0}")]
	Null(&'static str),
	#[error("{what} contains a NUL byte")]
	Nul { what: &'static str },
	#[error("reference Jsonnet returned non-UTF-8 output: {0}")]
	Utf8(#[from] std::string::FromUtf8Error),
	/// libjsonnet's own report, which already says what kind of error it is
	/// (`RUNTIME ERROR: …`, `STATIC ERROR: …`). It is shown as it is: this is
	/// also what an [`EvaluatorError`] turns back into, so a prefix here would
	/// be repeated every time an error crosses that boundary.
	#[error("{0}")]
	Evaluation(String),
}

#[derive(Clone, Copy)]
struct Api {
	make: unsafe extern "C" fn() -> *mut c_void,
	destroy: unsafe extern "C" fn(*mut c_void),
	realloc: unsafe extern "C" fn(*mut c_void, *mut c_char, usize) -> *mut c_char,
	evaluate_snippet:
		unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut c_int) -> *mut c_char,
	evaluate_file: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> *mut c_char,
	max_stack: unsafe extern "C" fn(*mut c_void, c_uint),
	jpath_add: unsafe extern "C" fn(*mut c_void, *const c_char),
	ext_var: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char),
	ext_code: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char),
	tla_var: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char),
	tla_code: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char),
	native_callback: unsafe extern "C" fn(
		*mut c_void,
		*const c_char,
		NativeCallback,
		*mut c_void,
		*const *const c_char,
	),
	extract_null: unsafe extern "C" fn(*mut c_void, *const c_void) -> c_int,
	extract_bool: unsafe extern "C" fn(*mut c_void, *const c_void) -> c_int,
	extract_number: unsafe extern "C" fn(*mut c_void, *const c_void, *mut f64) -> c_int,
	extract_string: unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_char,
	make_null: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
	make_bool: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void,
	make_number: unsafe extern "C" fn(*mut c_void, f64) -> *mut c_void,
	make_string: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
	make_array: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
	array_append: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
	make_object: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
	object_append: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char, *mut c_void),
	json_destroy: unsafe extern "C" fn(*mut c_void, *mut c_void),
}

/// The reference interpreter and its dynamically loaded C API.
///
/// Keep this alive for the lifetime of any [`Vm`] created from it. A library
/// without a matching version is refused rather than calling an unknown ABI.
#[derive(Clone)]
pub struct Implementation {
	#[allow(dead_code)] // Retains code and symbols until all VMs are dropped.
	library: Arc<Library>,
	api: Api,
}

impl fmt::Debug for Implementation {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Implementation").finish_non_exhaustive()
	}
}

impl Implementation {
	/// Open the system library, or the path in `RTK_JSONNET_REFERENCE_LIBRARY`.
	pub fn new() -> Result<Self, Error> {
		let path = std::env::var_os("RTK_JSONNET_REFERENCE_LIBRARY")
			.unwrap_or_else(|| OsStr::new(DEFAULT_LIBRARY).to_owned());
		Self::from_path(path)
	}

	/// Open a specific shared library; useful for explicit installations.
	pub fn from_path(path: impl AsRef<OsStr>) -> Result<Self, Error> {
		let path = path.as_ref();
		// SAFETY: loading a shared library can execute its initializers. The
		// caller chooses the path and we verify the expected API before use.
		let library = unsafe { Library::new(path) }.map_err(|source| Error::Load {
			path: path.to_string_lossy().into_owned(),
			source,
		})?;

		// SAFETY: jsonnet_version is part of the upstream public C ABI; no VM
		// or callback is needed. The library stays loaded through this call.
		let version = unsafe {
			let function: unsafe extern "C" fn() -> *const c_char = *library
				.get(b"jsonnet_version\0")
				.map_err(|source| Error::Symbol {
					symbol: "jsonnet_version",
					source,
				})?;
			let version = function();
			if version.is_null() {
				return Err(Error::Null("jsonnet_version"));
			}
			CStr::from_ptr(version).to_string_lossy().into_owned()
		};
		if version != VERSION {
			return Err(Error::Version { found: version });
		}

		// SAFETY: each name and signature matches the published v0.22.0 C
		// header. Copying function pointers does not borrow the Library; its
		// ownership is retained alongside the pointers in this struct.
		unsafe {
			macro_rules! symbol {
				($name:literal, $ty:ty) => {{
					*library
						.get::<$ty>(concat!($name, "\0").as_bytes())
						.map_err(|source| Error::Symbol {
							symbol: $name,
							source,
						})?
				}};
			}
			let api = Api {
				make: symbol!("jsonnet_make", unsafe extern "C" fn() -> *mut c_void),
				destroy: symbol!("jsonnet_destroy", unsafe extern "C" fn(*mut c_void)),
				realloc: symbol!(
					"jsonnet_realloc",
					unsafe extern "C" fn(*mut c_void, *mut c_char, usize) -> *mut c_char
				),
				evaluate_snippet: symbol!(
					"jsonnet_evaluate_snippet",
					unsafe extern "C" fn(
						*mut c_void,
						*const c_char,
						*const c_char,
						*mut c_int,
					) -> *mut c_char
				),
				evaluate_file: symbol!(
					"jsonnet_evaluate_file",
					unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> *mut c_char
				),
				max_stack: symbol!(
					"jsonnet_max_stack",
					unsafe extern "C" fn(*mut c_void, c_uint)
				),
				jpath_add: symbol!(
					"jsonnet_jpath_add",
					unsafe extern "C" fn(*mut c_void, *const c_char)
				),
				ext_var: symbol!(
					"jsonnet_ext_var",
					unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char)
				),
				ext_code: symbol!(
					"jsonnet_ext_code",
					unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char)
				),
				tla_var: symbol!(
					"jsonnet_tla_var",
					unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char)
				),
				tla_code: symbol!(
					"jsonnet_tla_code",
					unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char)
				),
				native_callback: symbol!(
					"jsonnet_native_callback",
					unsafe extern "C" fn(
						*mut c_void,
						*const c_char,
						NativeCallback,
						*mut c_void,
						*const *const c_char,
					)
				),
				extract_null: symbol!(
					"jsonnet_json_extract_null",
					unsafe extern "C" fn(*mut c_void, *const c_void) -> c_int
				),
				extract_bool: symbol!(
					"jsonnet_json_extract_bool",
					unsafe extern "C" fn(*mut c_void, *const c_void) -> c_int
				),
				extract_number: symbol!(
					"jsonnet_json_extract_number",
					unsafe extern "C" fn(*mut c_void, *const c_void, *mut f64) -> c_int
				),
				extract_string: symbol!(
					"jsonnet_json_extract_string",
					unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_char
				),
				make_null: symbol!(
					"jsonnet_json_make_null",
					unsafe extern "C" fn(*mut c_void) -> *mut c_void
				),
				make_bool: symbol!(
					"jsonnet_json_make_bool",
					unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void
				),
				make_number: symbol!(
					"jsonnet_json_make_number",
					unsafe extern "C" fn(*mut c_void, f64) -> *mut c_void
				),
				make_string: symbol!(
					"jsonnet_json_make_string",
					unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void
				),
				make_array: symbol!(
					"jsonnet_json_make_array",
					unsafe extern "C" fn(*mut c_void) -> *mut c_void
				),
				array_append: symbol!(
					"jsonnet_json_array_append",
					unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void)
				),
				make_object: symbol!(
					"jsonnet_json_make_object",
					unsafe extern "C" fn(*mut c_void) -> *mut c_void
				),
				object_append: symbol!(
					"jsonnet_json_object_append",
					unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char, *mut c_void)
				),
				json_destroy: symbol!(
					"jsonnet_json_destroy",
					unsafe extern "C" fn(*mut c_void, *mut c_void)
				),
			};
			Ok(Self {
				library: Arc::new(library),
				api,
			})
		}
	}

	/// Test support: the installed library, or [`None`] when there is none to
	/// test against.
	///
	/// A missing or differently versioned library is an ordinary machine and
	/// skips, unless `RTK_REQUIRE_REFERENCE_JSONNET` is set, which is how CI
	/// makes sure these tests ran rather than silently passing. A library that
	/// is the right version but lacks part of the API always fails.
	#[doc(hidden)]
	pub fn installed_for_tests() -> Option<Self> {
		match Self::new() {
			Ok(implementation) => Some(implementation),
			Err(error @ (Error::Load { .. } | Error::Version { .. })) => {
				assert!(
					std::env::var_os("RTK_REQUIRE_REFERENCE_JSONNET").is_none(),
					"RTK_REQUIRE_REFERENCE_JSONNET is set, but reference Jsonnet is unavailable: {error}"
				);
				None
			}
			Err(error) => panic!("installed reference Jsonnet has an incomplete API: {error}"),
		}
	}

	/// Create a VM that cannot outlive its loaded library.
	pub fn create_evaluator(&self) -> Result<Vm<'_>, Error> {
		// SAFETY: jsonnet_make takes no arguments and returns an owned VM.
		let vm = NonNull::new(unsafe { (self.api.make)() }).ok_or(Error::Null("jsonnet_make"))?;
		Ok(Vm {
			implementation: self,
			vm,
			registrations: Vec::new(),
			// Jsonnet VMs must not cross threads: the C API does not promise
			// thread-safe mutation or transfer of a live interpreter.
			_not_send: PhantomData,
		})
	}
}

/// A single reference Jsonnet VM. Operations use the library that created it.
pub struct Vm<'a> {
	implementation: &'a Implementation,
	vm: NonNull<c_void>,
	registrations: Vec<NativeRegistration>,
	_not_send: PhantomData<Rc<()>>,
}

struct NativeRegistration {
	name: CString,
	_params: Vec<CString>,
	param_pointers: Vec<*const c_char>,
}

impl Vm<'_> {
	fn string(value: &str, what: &'static str) -> Result<CString, Error> {
		CString::new(value).map_err(|_| Error::Nul { what })
	}

	fn path(path: &Path) -> Result<CString, Error> {
		Self::string(&path.to_string_lossy(), "path")
	}

	/// Set the maximum interpreter stack depth.
	pub fn max_stack(&mut self, depth: u32) {
		// SAFETY: the VM and its implementation are alive for this borrow.
		unsafe { (self.implementation.api.max_stack)(self.vm.as_ptr(), depth) }
	}

	/// Add an import search directory to this VM.
	pub fn add_import_path(&mut self, path: &Path) -> Result<(), Error> {
		let path = Self::path(path)?;
		// SAFETY: jsonnet_jpath_add copies the path into the VM.
		unsafe { (self.implementation.api.jpath_add)(self.vm.as_ptr(), path.as_ptr()) }
		Ok(())
	}

	fn bind(
		&mut self,
		key: &str,
		value: &str,
		function: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char),
	) -> Result<(), Error> {
		let key = Self::string(key, "binding name")?;
		let value = Self::string(value, "binding value")?;
		// SAFETY: jsonnet_{ext,tla}_{var,code} copy their arguments into the VM.
		unsafe { function(self.vm.as_ptr(), key.as_ptr(), value.as_ptr()) }
		Ok(())
	}

	pub fn with_external_variable(&mut self, key: &str, value: &str) -> Result<(), Error> {
		self.bind(key, value, self.implementation.api.ext_var)
	}

	pub fn with_external_code(&mut self, key: &str, code: &str) -> Result<(), Error> {
		self.bind(key, code, self.implementation.api.ext_code)
	}

	pub fn with_top_level_argument(&mut self, key: &str, value: &str) -> Result<(), Error> {
		self.bind(key, value, self.implementation.api.tla_var)
	}

	pub fn with_top_level_code(&mut self, key: &str, code: &str) -> Result<(), Error> {
		self.bind(key, code, self.implementation.api.tla_code)
	}

	/// Register a native callback using the reference C API.
	///
	/// # Safety
	///
	/// `context` must remain valid through the last callback invocation (including
	/// during VM destruction). `callback` must return values allocated by this VM's
	/// `jsonnet_json_make_*` functions, must not unwind, and must not retain `argv`.
	/// Each argument is forced before entry, and upstream accepts only primitive
	/// argument values even though callbacks may return compound JSON values.
	pub unsafe fn register_native(
		&mut self,
		name: &str,
		parameters: &[&str],
		callback: NativeCallback,
		context: *mut c_void,
	) -> Result<(), Error> {
		let name = Self::string(name, "native name")?;
		let params = parameters
			.iter()
			.map(|param| Self::string(param, "native parameter"))
			.collect::<Result<Vec<_>, _>>()?;
		let mut param_pointers = params
			.iter()
			.map(|param| param.as_ptr())
			.collect::<Vec<_>>();
		param_pointers.push(std::ptr::null());
		// Keep names and pointer arrays alive even if libjsonnet retains them.
		self.registrations.push(NativeRegistration {
			name,
			_params: params,
			param_pointers,
		});
		let registration = self.registrations.last().expect("just inserted");
		// SAFETY: the VM and parameter strings remain valid for the call and
		// the rest of the VM lifetime. The caller owns the callback contract.
		unsafe {
			(self.implementation.api.native_callback)(
				self.vm.as_ptr(),
				registration.name.as_ptr(),
				callback,
				context,
				registration.param_pointers.as_ptr(),
			);
		}
		Ok(())
	}

	fn output(&mut self, ptr: *mut c_char, failed: c_int) -> Result<String, Error> {
		let ptr = NonNull::new(ptr).ok_or(Error::Null("jsonnet_evaluate_*"))?;
		// SAFETY: a non-null evaluation result is a NUL-terminated buffer
		// allocated by this VM. Copy before freeing it with jsonnet_realloc.
		let bytes = unsafe { CStr::from_ptr(ptr.as_ptr()).to_bytes().to_vec() };
		unsafe { (self.implementation.api.realloc)(self.vm.as_ptr(), ptr.as_ptr(), 0) };
		let text = String::from_utf8(bytes)?;
		if failed != 0 {
			return Err(Error::Evaluation(text));
		}
		Ok(text)
	}

	/// Evaluate and fully manifest a snippet. The result is JSON text.
	pub fn evaluate_snippet(&mut self, filename: &str, snippet: &str) -> Result<String, Error> {
		let filename = Self::string(filename, "filename")?;
		let snippet = Self::string(snippet, "snippet")?;
		let mut failed = 0;
		// SAFETY: VM and input strings live through the call; the result is
		// released by output(), even when evaluation reports an error.
		let ptr = unsafe {
			(self.implementation.api.evaluate_snippet)(
				self.vm.as_ptr(),
				filename.as_ptr(),
				snippet.as_ptr(),
				&raw mut failed,
			)
		};
		self.output(ptr, failed)
	}

	/// Evaluate and fully manifest a file. The result is JSON text.
	pub fn evaluate_file(&mut self, filename: &Path) -> Result<String, Error> {
		let filename = Self::path(filename)?;
		let mut failed = 0;
		// SAFETY: VM and filename live through the call, and output() frees
		// the returned buffer using the same library and VM.
		let ptr = unsafe {
			(self.implementation.api.evaluate_file)(
				self.vm.as_ptr(),
				filename.as_ptr(),
				&raw mut failed,
			)
		};
		self.output(ptr, failed)
	}
}

impl Drop for Vm<'_> {
	fn drop(&mut self) {
		// SAFETY: the VM is uniquely owned and its library outlives this Vm.
		unsafe { (self.implementation.api.destroy)(self.vm.as_ptr()) }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn missing_library_is_reported() {
		let error = Implementation::from_path("/nonexistent/rtk-reference-jsonnet.so")
			.expect_err("missing library");
		assert!(matches!(error, Error::Load { .. }), "{error}");
	}

	struct NumberCallback {
		vm: *mut c_void,
		extract: unsafe extern "C" fn(*mut c_void, *const c_void, *mut f64) -> c_int,
		make: unsafe extern "C" fn(*mut c_void, f64) -> *mut c_void,
		make_string: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
	}

	unsafe extern "C" fn add_numbers(
		context: *mut c_void,
		arguments: *const *const c_void,
		success: *mut c_int,
	) -> *mut c_void {
		// SAFETY: register_native's caller keeps this context alive until the VM
		// and callback are finished; the registration declares two parameters.
		let context = unsafe { &*context.cast::<NumberCallback>() };
		let mut left = 0.0;
		let mut right = 0.0;
		if unsafe { (context.extract)(context.vm, *arguments, &raw mut left) } != 1
			|| unsafe { (context.extract)(context.vm, *arguments.add(1), &raw mut right) } != 1
		{
			unsafe { *success = 0 };
			return unsafe {
				(context.make_string)(context.vm, c"arguments must be numbers".as_ptr())
			};
		}
		unsafe { *success = 1 };
		unsafe { (context.make)(context.vm, left + right) }
	}

	#[test]
	fn native_callbacks_use_the_reference_library() {
		let Some(implementation) = Implementation::installed_for_tests() else {
			return;
		};
		// SAFETY: the installed version matches the header whose signatures are
		// written here; the loaded library outlives both pointers and the VM.
		let extract = unsafe {
			*implementation
				.library
				.get::<unsafe extern "C" fn(*mut c_void, *const c_void, *mut f64) -> c_int>(
					b"jsonnet_json_extract_number\0",
				)
				.expect("number extractor")
		};
		let make = unsafe {
			*implementation
				.library
				.get::<unsafe extern "C" fn(*mut c_void, f64) -> *mut c_void>(
					b"jsonnet_json_make_number\0",
				)
				.expect("number constructor")
		};
		let make_string = unsafe {
			*implementation
				.library
				.get::<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>(
					b"jsonnet_json_make_string\0",
				)
				.expect("string constructor")
		};
		let mut context = Box::new(NumberCallback {
			vm: std::ptr::null_mut(),
			extract,
			make,
			make_string,
		});
		let mut evaluator = implementation.create_evaluator().expect("VM");
		context.vm = evaluator.vm.as_ptr();
		// SAFETY: the context and its functions outlive the VM even on panic;
		// add_numbers does not unwind or retain argv.
		unsafe {
			evaluator.register_native(
				"add",
				&["a", "b"],
				add_numbers,
				std::ptr::from_mut(&mut *context).cast(),
			)
		}
		.expect("register native");
		assert_eq!(
			evaluator
				.evaluate_snippet("test.jsonnet", "std.native('add')(1, 2)")
				.expect("native evaluation")
				.trim(),
			"3"
		);
		let error = evaluator
			.evaluate_snippet("test.jsonnet", "std.native('add')(1, error 'forced')")
			.expect_err("native arguments are eagerly forced");
		assert!(error.to_string().contains("forced"), "{error}");
		drop(evaluator);
	}

	#[test]
	fn evaluates_using_installed_reference_library() {
		let Some(implementation) = Implementation::installed_for_tests() else {
			return;
		};
		let mut evaluator = implementation.create_evaluator().expect("VM");
		evaluator
			.with_external_variable("subject", "world")
			.expect("extVar");
		assert_eq!(
			evaluator
				.evaluate_snippet("test.jsonnet", "{hello: std.extVar('subject')}")
				.expect("evaluation")
				.trim(),
			"{\n   \"hello\": \"world\"\n}"
		);
		let error = evaluator
			.evaluate_snippet("test.jsonnet", "{good: 1, bad: error 'do not force'}")
			.expect_err("the public C API manifests every visible field");
		assert!(error.to_string().contains("do not force"), "{error}");
	}
}
