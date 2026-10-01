use std::collections::HashMap;
use std::convert::Infallible;
use std::ffi::{CStr, CString, c_int, c_void};
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc as Shared;
use std::sync::{LazyLock, Mutex, PoisonError};

use rtk_jsonnet_core as core;
use rtk_jsonnet_core::FlagsExt;
use rtk_spec::canonical::{
	EnvironmentSpec, JsonentImplementationOrConfig, JsonnetImplementation, Rc,
};
use serde_json::Value as Json;

use crate::native::{Arguments, Value};
use crate::{Api, Error, Implementation, Vm};

#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct EvaluatorError(pub String);
impl From<Error> for EvaluatorError {
	fn from(error: Error) -> Self {
		Self(error.to_string())
	}
}
impl From<serde_json::Error> for EvaluatorError {
	fn from(error: serde_json::Error) -> Self {
		Self(error.to_string())
	}
}
impl core::EvaluatorError for EvaluatorError {
	type Evaluator = Evaluator;
	fn custom<T: fmt::Display>(message: T) -> Self {
		Self(message.to_string())
	}
}

#[derive(Debug, thiserror::Error)]
#[error("unsupported reference Jsonnet flag: {0}")]
pub struct FlagError(String);
#[derive(Debug)]
pub struct Flag;
impl core::Flag for Flag {
	type Implementation = Implementation;
	type Key = String;
	type Value = String;
	type Error = FlagError;
	fn new(key: String, _: String) -> Result<Self, Self::Error> {
		Err(FlagError(key))
	}
}
impl From<Infallible> for FlagError {
	fn from(never: Infallible) -> Self {
		match never {}
	}
}
impl From<FlagError> for Error {
	fn from(error: FlagError) -> Self {
		Error::Evaluation(error.to_string())
	}
}
impl From<EvaluatorError> for Error {
	fn from(error: EvaluatorError) -> Self {
		Error::Evaluation(error.to_string())
	}
}
impl core::Implementation for Implementation {
	type Evaluator = Evaluator;
	type Flag = Flag;
	type Error = Error;
	type InitializationError = Error;
	fn new(mut flags: impl Iterator<Item = Flag>) -> Result<Self, Error> {
		// A flag is only constructible by returning an error, so a nonempty
		// iterator cannot originate from the core flag parser.
		if flags.next().is_some() {
			return Err(Error::Evaluation(
				"unsupported reference Jsonnet flag".into(),
			));
		}
		Implementation::new()
	}
}

type NativeCall = dyn Fn(&Evaluator, Arguments) -> Result<Value, EvaluatorError>;

struct Native {
	name: String,
	parameters: Vec<String>,
	call: Box<NativeCall>,
}

#[derive(Clone)]
pub struct Evaluator {
	implementation: Implementation,
	import_paths: Vec<PathBuf>,
	max_stack: Option<u32>,
	external_code: Vec<(String, String)>,
	external_variables: Vec<(String, String)>,
	top_level_code: Vec<(String, String)>,
	top_level_arguments: Vec<(String, String)>,
	natives: Vec<Shared<Native>>,
	disable_natives: bool,
}
impl core::Context for Evaluator {
	type Evaluator = Self;
}

struct CallbackContext {
	evaluator: Evaluator,
	native: Shared<Native>,
	vm: *mut c_void,
	api: Api,
}

impl CallbackContext {
	/// Read a primitive input from the public C API. It cannot expose arrays or
	/// objects passed as native arguments (libjsonnet rejects those itself).
	unsafe fn argument(&self, ptr: *const c_void) -> Result<Value, EvaluatorError> {
		let api = self.api;
		let vm = self.vm;
		if unsafe { (api.extract_null)(vm, ptr) } == 1 {
			return Ok(Value(Json::Null));
		}
		match unsafe { (api.extract_bool)(vm, ptr) } {
			0 => return Ok(Value(Json::Bool(false))),
			1 => return Ok(Value(Json::Bool(true))),
			_ => {}
		}
		let mut number = 0.0;
		if unsafe { (api.extract_number)(vm, ptr, &raw mut number) } == 1 {
			// The C API represents every Jsonnet number as f64. Serde's
			// integer visitors still need to receive integral values as integers.
			#[allow(clippy::cast_possible_truncation)]
			let json = if number.fract() == 0.0
				&& (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&number)
			{
				Json::from(number as i64)
			} else {
				Json::from(number)
			};
			return Ok(Value(json));
		}
		let string = unsafe { (api.extract_string)(vm, ptr) };
		if !string.is_null() {
			return Ok(Value(Json::String(
				unsafe { CStr::from_ptr(string) }
					.to_string_lossy()
					.into_owned(),
			)));
		}
		Err(EvaluatorError("unsupported native argument type".into()))
	}

	/// Construct a result using only JSON constructors owned by this VM.
	///
	/// A container takes ownership of what is appended to it, so a failure part
	/// way through destroys the container built so far rather than leaking it.
	fn result(&self, value: &Json) -> Result<*mut c_void, EvaluatorError> {
		let api = self.api;
		let vm = self.vm;
		// SAFETY: this context is only used while its VM and loaded library live.
		let ptr = unsafe {
			match value {
				Json::Null => (api.make_null)(vm),
				Json::Bool(b) => (api.make_bool)(vm, i32::from(*b)),
				Json::Number(n) => (api.make_number)(vm, n.as_f64().expect("JSON number")),
				Json::String(s) => {
					let text = Self::c_string(s, "native result contains NUL")?;
					(api.make_string)(vm, text.as_ptr())
				}
				Json::Array(values) => {
					let array = Self::non_null((api.make_array)(vm))?;
					for value in values {
						let element = self
							.result(value)
							.inspect_err(|_| (api.json_destroy)(vm, array))?;
						(api.array_append)(vm, array, element);
					}
					array
				}
				Json::Object(fields) => {
					let object = Self::non_null((api.make_object)(vm))?;
					for (key, value) in fields {
						let field = Self::c_string(key, "native object key contains NUL")
							.and_then(|key| Ok((key, self.result(value)?)))
							.inspect_err(|_| (api.json_destroy)(vm, object))?;
						(api.object_append)(vm, object, field.0.as_ptr(), field.1);
					}
					object
				}
			}
		};
		Self::non_null(ptr)
	}

	fn c_string(text: &str, failure: &str) -> Result<CString, EvaluatorError> {
		CString::new(text).map_err(|_| EvaluatorError(failure.into()))
	}

	fn non_null(ptr: *mut c_void) -> Result<*mut c_void, EvaluatorError> {
		if ptr.is_null() {
			return Err(EvaluatorError(
				"reference Jsonnet returned a null JSON value pointer".into(),
			));
		}
		Ok(ptr)
	}
	fn error(&self, message: &str) -> *mut c_void {
		let message =
			CString::new(message).unwrap_or_else(|_| c"native callback failed".to_owned());
		// SAFETY: this VM is alive during callback execution.
		unsafe { (self.api.make_string)(self.vm, message.as_ptr()) }
	}
}

unsafe extern "C" fn native_callback(
	context: *mut c_void,
	argv: *const *const c_void,
	success: *mut c_int,
) -> *mut c_void {
	// SAFETY: registration keeps context alive and libjsonnet passes exactly
	// as many arguments as the registered parameter list declares.
	let context = unsafe { &*context.cast::<CallbackContext>() };
	let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		let args = (0..context.native.parameters.len())
			.map(|i| {
				let arg = unsafe { *argv.add(i) };
				unsafe { context.argument(arg) }
			})
			.collect::<Result<Vec<_>, _>>()?;
		let value = (context.native.call)(&context.evaluator, Arguments(args))?;
		context.result(&value.0)
	}));
	match result {
		Ok(Ok(value)) => {
			unsafe { *success = 1 };
			value
		}
		Ok(Err(error)) => {
			unsafe { *success = 0 };
			context.error(&error.to_string())
		}
		Err(_) => {
			unsafe { *success = 0 };
			context.error("native callback panicked")
		}
	}
}

/// Process-global, as the jrsonnet implementation's is: the cache exists to
/// cross evaluations, and a per-thread one would make which value a key holds
/// depend on which worker happened to evaluate an environment.
///
/// libjsonnet forces native arguments before the call, so nothing is saved
/// here — the value has been computed either way. What is kept is the meaning:
/// the first value stored under a key is the one every later call receives.
static MEMO: LazyLock<Mutex<HashMap<String, Json>>> = LazyLock::new(Mutex::default);

struct Memoize;
impl core::Function<Evaluator> for Memoize {
	fn argv(&self) -> (usize, Option<usize>) {
		(2, None)
	}
	fn parameter_names(&self) -> Option<&'static [&'static str]> {
		Some(&["key", "value"])
	}
	fn call(&self, _: &Evaluator, args: Arguments) -> Result<Value, EvaluatorError> {
		let [key, value] = <[Value; 2]>::try_from(args.0)
			.map_err(|_| EvaluatorError("rtkMemoize needs two arguments".into()))?;
		let key = key
			.0
			.as_str()
			.ok_or_else(|| EvaluatorError("rtkMemoize key must be a string".into()))?;
		let mut memo = MEMO.lock().unwrap_or_else(PoisonError::into_inner);
		Ok(Value(memo.entry(key.to_owned()).or_insert(value.0).clone()))
	}
}

impl Evaluator {
	/// Whether flags configured with `implementation` are addressed to this one.
	///
	/// A project or environment may configure another implementation's flags
	/// and still be evaluated here, when something more specific chose the
	/// reference interpreter. Those flags are not this implementation's to
	/// refuse.
	fn configures_reference(implementation: Option<&JsonentImplementationOrConfig>) -> bool {
		implementation.is_some_and(|implementation| {
			matches!(
				implementation.implementation(),
				JsonnetImplementation::Reference
			)
		})
	}

	fn evaluate(
		self,
		run: impl FnOnce(&mut Vm<'_>) -> Result<String, Error>,
	) -> Result<(Self, Value), EvaluatorError> {
		// Contexts outlive the VM, including its destruction. Each context owns
		// a clone of the evaluator so Function::call can use a valid reference.
		let mut contexts: Vec<Box<CallbackContext>> = Vec::new();
		let mut vm = self.implementation.create_evaluator()?;
		if let Some(depth) = self.max_stack {
			vm.max_stack(depth);
		}
		// `import_paths` is in order of precedence, first wins. libjsonnet searches
		// the path it was given last first (the `jsonnet` CLI's "right-most wins"),
		// so the paths are handed over reversed.
		for path in self.import_paths.iter().rev() {
			vm.add_import_path(path)?;
		}
		for (key, value) in &self.external_code {
			vm.with_external_code(key, value)?;
		}
		for (key, value) in &self.external_variables {
			vm.with_external_variable(key, value)?;
		}
		for (key, value) in &self.top_level_code {
			vm.with_top_level_code(key, value)?;
		}
		for (key, value) in &self.top_level_arguments {
			vm.with_top_level_argument(key, value)?;
		}
		if !self.disable_natives {
			for native in &self.natives {
				let mut context = Box::new(CallbackContext {
					evaluator: self.clone(),
					native: native.clone(),
					vm: vm.vm.as_ptr(),
					api: self.implementation.api,
				});
				let params = native
					.parameters
					.iter()
					.map(String::as_str)
					.collect::<Vec<_>>();
				// SAFETY: context is heap allocated and remains alive until the
				// VM is destroyed; callback catches Rust panics at the ABI.
				unsafe {
					vm.register_native(
						&native.name,
						&params,
						native_callback,
						(&raw mut *context).cast(),
					)?;
				}
				contexts.push(context);
			}
		}
		let output = run(&mut vm);
		drop(vm);
		drop(contexts);
		let output = output?;
		Ok((self, Value(serde_json::from_str(&output)?)))
	}
}
impl core::Evaluator for Evaluator {
	type Implementation = Implementation;
	type Arguments = Arguments;
	type Context = Self;
	type Error = EvaluatorError;
	type Value = Value;
	fn new(implementation: &Implementation) -> Self {
		Self {
			implementation: implementation.clone(),
			import_paths: Vec::new(),
			max_stack: None,
			external_code: Vec::new(),
			external_variables: Vec::new(),
			top_level_code: Vec::new(),
			top_level_arguments: Vec::new(),
			natives: Vec::new(),
			disable_natives: false,
		}
	}
	fn with_rc(&mut self, rc: Rc) -> Result<&mut Self, EvaluatorError> {
		if Self::configures_reference(rc.spec.jsonnet_implementation.as_ref()) {
			let _ = rc
				.flags::<Flag>()
				.map_err(|error| EvaluatorError(error.to_string()))?;
		}
		if let Some(depth) = rc.spec.max_stack_depth {
			self.max_stack = Some(
				u32::try_from(depth)
					.map_err(|_| EvaluatorError("maxStackDepth exceeds u32".into()))?,
			);
		}
		self.disable_natives = rc.spec.disable_native_functions;
		Ok(self)
	}
	fn with_environment(
		&mut self,
		environment: &EnvironmentSpec,
	) -> Result<&mut Self, EvaluatorError> {
		if Self::configures_reference(environment.export_jsonnet_implementation.as_ref()) {
			let _ = environment
				.flags::<Flag>()
				.map_err(|error| EvaluatorError(error.to_string()))?;
		}
		Ok(self)
	}
	fn with_import_paths(&mut self, paths: Vec<PathBuf>) -> Result<&mut Self, EvaluatorError> {
		self.import_paths = paths;
		Ok(self)
	}
	fn with_plugin<P: core::Plugin<Self>>(
		&mut self,
		plugin: P,
	) -> Result<&mut Self, EvaluatorError> {
		plugin.install(self)?;
		Ok(self)
	}
	fn with_external_code(&mut self, key: &str, value: &str) -> Result<&mut Self, EvaluatorError> {
		self.external_code.push((key.into(), value.into()));
		Ok(self)
	}
	fn with_external_variable(
		&mut self,
		key: &str,
		value: &str,
	) -> Result<&mut Self, EvaluatorError> {
		self.external_variables.push((key.into(), value.into()));
		Ok(self)
	}
	fn with_native_function<F: 'static + core::Function<Self>>(
		&mut self,
		key: &str,
		func: F,
	) -> Result<&mut Self, EvaluatorError> {
		let (total, optional) = func.argv();
		if optional.unwrap_or(0) > 0 {
			return Err(EvaluatorError(
				"the reference Jsonnet C API does not support optional native parameters".into(),
			));
		}
		let parameters = if let Some(names) = func.parameter_names() {
			if names.len() != total {
				return Err(EvaluatorError(
					"native parameter name count differs from arity".into(),
				));
			}
			names.iter().map(|s| (*s).to_owned()).collect()
		} else {
			(0..total).map(|i| format!("arg{i}")).collect()
		};
		self.natives.push(Shared::new(Native {
			name: key.into(),
			parameters,
			call: Box::new(move |evaluator, args| func.call(evaluator, args)),
		}));
		Ok(self)
	}
	fn with_rtk_memoize(&mut self) -> Result<&mut Self, EvaluatorError> {
		self.with_native_function("rtkMemoize", Memoize)
	}
	fn with_top_level_argument(
		&mut self,
		key: &str,
		value: &str,
	) -> Result<&mut Self, EvaluatorError> {
		self.top_level_arguments.push((key.into(), value.into()));
		Ok(self)
	}
	fn with_top_level_code(&mut self, key: &str, value: &str) -> Result<&mut Self, EvaluatorError> {
		self.top_level_code.push((key.into(), value.into()));
		Ok(self)
	}
	fn evaluate_file<P: AsRef<Path> + fmt::Debug>(
		self,
		path: P,
	) -> Result<(Self, Value), EvaluatorError> {
		self.evaluate(|vm| vm.evaluate_file(path.as_ref()))
	}
	fn evaluate_snippet<S: AsRef<str> + fmt::Debug>(
		self,
		snippet: S,
	) -> Result<(Self, Value), EvaluatorError> {
		self.evaluate(|vm| vm.evaluate_snippet("<anonymous>", snippet.as_ref()))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use rtk_jsonnet_core::{Array as _, Context as _, Evaluator as _, Object as _, Value as _};
	use serde::{Deserialize, Serialize};

	fn implementation() -> Option<Implementation> {
		Implementation::installed_for_tests()
	}

	#[derive(Debug, Deserialize, Serialize, PartialEq)]
	struct Data {
		name: String,
		numbers: Vec<i32>,
	}

	#[test]
	fn core_evaluation_and_serde_round_trip() {
		let Some(implementation) = implementation() else {
			return;
		};
		let mut evaluator = core::Implementation::create_evaluator(&implementation);
		evaluator.with_external_variable("name", "world").unwrap();
		let (context, value) = evaluator
			.evaluate_snippet("{name: std.extVar('name'), numbers: [1, 2], secret:: 4}")
			.unwrap();
		let object = value.as_object().unwrap();
		assert!(object.has("numbers", core::Hidden::Skip).unwrap());
		assert!(!object.has("secret", core::Hidden::Include).unwrap());
		assert_eq!(
			object
				.get("numbers", core::Hidden::Skip)
				.unwrap()
				.unwrap()
				.as_array()
				.unwrap()
				.get(1)
				.unwrap()
				.unwrap()
				.as_number(),
			Some(2.0)
		);
		let data = Data::deserialize(context.create_deserializer(value.clone())).unwrap();
		assert_eq!(
			data,
			Data {
				name: "world".into(),
				numbers: vec![1, 2]
			}
		);
		let encoded = data.serialize(context.create_serializer()).unwrap();
		assert_eq!(encoded.manifest().unwrap(), value.manifest().unwrap());
		let raw: core::RawValue<Value> =
			Deserialize::deserialize(context.create_deserializer(value.clone())).unwrap();
		let transferred = raw.serialize(context.create_serializer()).unwrap();
		assert_eq!(transferred.manifest().unwrap(), value.manifest().unwrap());
	}

	struct Add;
	impl core::Function<Evaluator> for Add {
		fn argv(&self) -> (usize, Option<usize>) {
			(2, None)
		}
		fn call(&self, _: &Evaluator, args: Arguments) -> Result<Value, EvaluatorError> {
			let (left, right): (i32, i32) = Deserialize::deserialize(args)?;
			Ok(Value(Json::from(left + right)))
		}
	}

	#[test]
	fn core_native_callback_and_error() {
		let Some(implementation) = implementation() else {
			return;
		};
		let mut evaluator = core::Implementation::create_evaluator(&implementation);
		evaluator.with_native_function("add", Add).unwrap();
		let (_, value) = evaluator
			.clone()
			.evaluate_snippet("std.native('add')(2, 3)")
			.unwrap();
		assert_eq!(value.as_number(), Some(5.0));
		let error = evaluator
			.evaluate_snippet("std.native('add')('wrong', 2)")
			.err()
			.expect("native failure");
		assert!(error.to_string().contains("invalid type"), "{error}");
	}

	struct Negate;
	impl core::Function<Evaluator> for Negate {
		fn argv(&self) -> (usize, Option<usize>) {
			(1, None)
		}
		fn call(&self, _: &Evaluator, args: Arguments) -> Result<Value, EvaluatorError> {
			let (value,): (bool,) = Deserialize::deserialize(args)?;
			Ok(Value(Json::Bool(!value)))
		}
	}

	#[test]
	fn native_boolean_arguments_use_the_public_abi() {
		let Some(implementation) = implementation() else {
			return;
		};
		let mut evaluator = core::Implementation::create_evaluator(&implementation);
		evaluator.with_native_function("negate", Negate).unwrap();
		for (input, expected) in [("true", false), ("false", true)] {
			let (_, value) = evaluator
				.clone()
				.evaluate_snippet(format!("std.native('negate')({input})"))
				.unwrap();
			assert_eq!(value.as_bool(), Some(expected));
		}
	}

	struct Wrap;
	impl core::Function<Evaluator> for Wrap {
		fn argv(&self) -> (usize, Option<usize>) {
			(1, None)
		}
		fn call(&self, _: &Evaluator, args: Arguments) -> Result<Value, EvaluatorError> {
			let [value] = <[Value; 1]>::try_from(args.0).expect("one parameter");
			Ok(Value(serde_json::json!({"wrapped": [value.0, null, true]})))
		}
	}

	#[test]
	fn native_can_return_compound_values_and_rejects_compound_arguments() {
		let Some(implementation) = implementation() else {
			return;
		};
		let mut evaluator = core::Implementation::create_evaluator(&implementation);
		evaluator.with_native_function("wrap", Wrap).unwrap();
		let (_, value) = evaluator
			.clone()
			.evaluate_snippet("std.native('wrap')('hi')")
			.unwrap();
		assert_eq!(value.0, serde_json::json!({"wrapped": ["hi", null, true]}));
		let error = evaluator
			.evaluate_snippet("std.native('wrap')([1, 2])")
			.err()
			.expect("compound input fails");
		assert!(
			error.to_string().contains("primitive") || error.to_string().contains("type"),
			"{error}"
		);
	}

	#[derive(Debug, Deserialize, PartialEq)]
	enum Choice {
		One,
		Two { count: i32 },
	}
	#[test]
	fn serde_enums() {
		let Some(implementation) = implementation() else {
			return;
		};
		let (context, value) = core::Implementation::create_evaluator(&implementation)
			.evaluate_snippet("[{One: null}, {Two: {count: 2}}]")
			.unwrap();
		let choices = Vec::<Choice>::deserialize(context.create_deserializer(value)).unwrap();
		assert_eq!(choices, vec![Choice::One, Choice::Two { count: 2 }]);
	}

	#[test]
	fn the_first_import_path_wins() {
		let Some(implementation) = implementation() else {
			return;
		};
		let directory = tempfile::tempdir().unwrap();
		let paths = ["first", "second", "third"].map(|name| {
			let path = directory.path().join(name);
			std::fs::create_dir(&path).unwrap();
			std::fs::write(path.join("x.libsonnet"), format!("'{name}'")).unwrap();
			path
		});
		let mut evaluator = core::Implementation::create_evaluator(&implementation);
		evaluator.with_import_paths(paths.to_vec()).unwrap();
		let (_, value) = evaluator.evaluate_snippet("import 'x.libsonnet'").unwrap();
		assert_eq!(value.as_str().as_deref(), Some("first"));
	}

	#[test]
	fn errors_are_libjsonnets_own_report() {
		let Some(implementation) = implementation() else {
			return;
		};
		let error = core::Implementation::create_evaluator(&implementation)
			.evaluate_snippet("error 'boom'")
			.err()
			.expect("an error");
		let error = Error::from(error).to_string();
		assert!(error.starts_with("RUNTIME ERROR: boom"), "{error}");
	}

	struct Nested;
	impl core::Function<Evaluator> for Nested {
		fn argv(&self) -> (usize, Option<usize>) {
			(0, None)
		}
		fn call(&self, _: &Evaluator, _: Arguments) -> Result<Value, EvaluatorError> {
			Ok(Value(serde_json::json!({"good": [1], "bad\u{0}key": 2})))
		}
	}

	#[test]
	fn a_result_that_cannot_be_built_is_an_error() {
		let Some(implementation) = implementation() else {
			return;
		};
		let mut evaluator = core::Implementation::create_evaluator(&implementation);
		evaluator.with_native_function("nested", Nested).unwrap();
		let error = evaluator
			.evaluate_snippet("std.native('nested')()")
			.err()
			.expect("a NUL in a key cannot cross the C API");
		assert!(error.to_string().contains("contains NUL"), "{error}");
	}

	#[test]
	fn memoized_values_are_shared_across_threads() {
		let Some(implementation) = implementation() else {
			return;
		};
		let evaluate = move |value: u32| {
			let mut evaluator = core::Implementation::create_evaluator(&implementation);
			evaluator.with_rtk_memoize().unwrap();
			let (_, value) = evaluator
				.evaluate_snippet(format!(
					"std.native('rtkMemoize')('reference-memo-threads', {value})"
				))
				.unwrap();
			value.as_number()
		};
		let first = evaluate(1);
		let second = std::thread::spawn(move || evaluate(2)).join().unwrap();
		assert_eq!(first, Some(1.0));
		assert_eq!(second, Some(1.0));
	}

	#[test]
	fn another_implementations_flags_are_not_refused() {
		let Some(implementation) = implementation() else {
			return;
		};
		let configured = |implementation: &str| -> EnvironmentSpec {
			serde_json::from_value(serde_json::json!({
				"exportJsonnetImplementation": {
					"type": implementation,
					"flags": {"some-flag": "value"},
				},
			}))
			.unwrap()
		};
		let mut evaluator = core::Implementation::create_evaluator(&implementation);
		evaluator
			.with_environment(&configured("jrsonnet"))
			.expect("jrsonnet's flags are not the reference interpreter's to refuse");
		let error = evaluator
			.with_environment(&configured("c++"))
			.err()
			.expect("a flag addressed to the reference interpreter is refused");
		assert!(error.to_string().contains("some-flag"), "{error}");
	}

	#[test]
	fn core_eager_manifest_reports_bad_fields() {
		let Some(implementation) = implementation() else {
			return;
		};
		let error = core::Implementation::create_evaluator(&implementation)
			.evaluate_snippet("{good: 1, bad: error 'forced'}")
			.err()
			.expect("eager failure");
		assert!(error.to_string().contains("forced"), "{error}");
	}
}
