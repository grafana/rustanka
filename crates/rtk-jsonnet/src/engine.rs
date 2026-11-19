use crate::Evaluation;
use std::convert::Infallible;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, Weak};

use rtk_jsonnet_core::{Evaluator as _, FlagsExt, Function, Implementation, ScopedEvaluator as _};
use rtk_jsonnet_jrsonnet::{
	Error as JrsonnetError, Evaluator as JrsonnetEvaluator,
	EvaluatorError as JrsonnetEvaluatorError, Flag as JrsonnetFlag,
	Implementation as JrsonnetImplementation,
};
use rtk_jsonnet_reference::{
	Evaluator as ReferenceEvaluator, Implementation as ReferenceImplementation,
};
use rtk_spec::DeepMerge;
use rtk_spec::canonical::{EnvironmentSpec, JsonnetImplementation, Rc};
use rustc_hash::FxHashMap;
use thiserror::Error;

use crate::jpath::JPath;

/// An error returned by one of the various Jsonnet implementations.
#[derive(Debug, Error)]
pub enum Error {
	/// Renders as the error underneath it: which implementation raised it is
	/// already clear from what it says, and is not the reason for it.
	#[error(transparent)]
	Jrsonnet(#[from] JrsonnetError),

	#[error(transparent)]
	Reference(#[from] rtk_jsonnet_reference::Error),

	#[error(
		"a jrsonnet-only native function cannot be installed on the reference Jsonnet evaluator"
	)]
	JrsonnetNativeOnReference,

	#[error("a jrsonnet context is unavailable for the reference Jsonnet evaluator")]
	JrsonnetContextOnReference,

	#[error("reading {path}: {source}")]
	Rc {
		path: String,
		#[source]
		source: rtk_spec::canonical::RcError,
	},

	#[error(
		"parsing version constraint: '{reason}'. Please check 'expectVersions.tanka' in {path}"
	)]
	UnreadableProjectTankaConstraint { path: String, reason: String },

	#[error(
		"current version '{}' does not satisfy the version required by the project: '{constraint}'. You likely need to use another version of Tanka",
		rtk_masterminds::TANKA_COMPATIBLE_VERSION
	)]
	UnsatisfiedProjectTankaVersion { constraint: String },

	#[error("could not read the installed helm version: {reason}")]
	UnreadableHelmVersion { reason: String },

	#[error(
		"helm {installed} is installed, but the project expects helm {expected}. Please check 'expectVersions.helm'"
	)]
	UnsatisfiedHelmVersion { expected: u64, installed: u64 },
}

impl From<Infallible> for Error {
	fn from(_: Infallible) -> Self {
		unreachable!()
	}
}

impl From<rtk_jsonnet_reference::EvaluatorError> for Error {
	fn from(error: rtk_jsonnet_reference::EvaluatorError) -> Self {
		rtk_jsonnet_reference::Error::from(error).into()
	}
}

impl From<JrsonnetEvaluatorError> for Error {
	fn from(error: JrsonnetEvaluatorError) -> Self {
		JrsonnetError::Evaluator(error).into()
	}
}

#[derive(Clone, Debug)]
pub struct Engine(Arc<EngineInternals>);

impl Engine {
	pub fn new(options: Options) -> Engine {
		let helm = if options.helm_cache {
			rtk_jsonnet_helm::Plugin::with_disk_cache(helm_cache_directory)
		} else {
			rtk_jsonnet_helm::Plugin::new()
		};
		Engine(Arc::new_cyclic(move |engine| EngineInternals {
			options,
			helm,
			implementations: RwLock::new(Implementations {
				engine: engine.clone(),
				..Default::default()
			}),
		}))
	}

	pub fn create_evaluator(&self) -> Evaluator {
		self.create_evaluator_for(None)
	}

	/// Create an evaluator for a particular environment.
	///
	/// An environment may ask to be evaluated by another Jsonnet implementation,
	/// which decides both how the result is formatted and whether Tanka's native
	/// functions exist at all — so it has to be known before the evaluator is
	/// configured, not applied to one afterwards. Taking it here is what makes
	/// that impossible to get wrong.
	pub fn create_evaluator_for(&self, environment: Option<&EnvironmentSpec>) -> Evaluator {
		Evaluator {
			engine: self.0.clone(),
			options: self.0.options.clone(),
			environment: environment.cloned(),
			evaluator: None,
		}
	}

	pub fn options(&self) -> &Options {
		&self.0.options
	}
}

#[derive(Debug)]
struct EngineInternals {
	options: Options,
	helm: rtk_jsonnet_helm::Plugin,
	implementations: RwLock<Implementations>,
}

fn helm_cache_directory(called_from: &Path) -> Option<PathBuf> {
	let root = JPath::project_root(called_from).ok()?;
	let root = root.canonicalize().unwrap_or(root);
	Some(root.join("target").join("helm"))
}

#[derive(Debug)]
pub struct Evaluator {
	engine: Arc<EngineInternals>,
	options: Options,
	environment: Option<EnvironmentSpec>,
	evaluator: Option<ImplementationEvaluator>,
}

/// A configured jrsonnet context for integrations with a custom `State`.
#[derive(Clone)]
pub struct JrsonnetContext(rtk_jsonnet_jrsonnet::Evaluator);

impl JrsonnetContext {
	pub fn context_initializer(&self) -> rtk_jsonnet_jrsonnet::ContextInitializer {
		self.0.context_initializer()
	}

	pub fn with_current<T>(&self, callback: impl FnOnce() -> T) -> T {
		self.0.with_current(callback)
	}
}

macro_rules! call_implementation_evaluator_method {
    ($self:ident, $method:ident, $($argument:expr),* $(,)?) => {
        match &mut $self.evaluator {
            Some(ImplementationEvaluator::Jrsonnet(jrsonnet)) => { jrsonnet.$method($($argument),*)?; }
            Some(ImplementationEvaluator::Reference(reference)) => { reference.$method($($argument),*)?; }
            None => {
                let implementation = $self.selected_implementation();
                match $self.populate_evaluator(implementation)? {
                    ImplementationEvaluator::Jrsonnet(jrsonnet) => { jrsonnet.$method($($argument),*)?; }
                    ImplementationEvaluator::Reference(reference) => { reference.$method($($argument),*)?; }
                }
            },
        }
    };
    (@no_insert: $self:ident, $method:ident, $($argument:expr),* $(,)?) => {
        match &mut $self.evaluator {
            Some(ImplementationEvaluator::Jrsonnet(jrsonnet)) => { jrsonnet.$method($($argument),*)?; }
            Some(ImplementationEvaluator::Reference(reference)) => { reference.$method($($argument),*)?; }
            None => panic!("attempt to use evaluator before population"),
        }
    };
    (@with: $evaluator:ident, $method:ident, $($argument:expr),* $(,)?) => {
        match $evaluator {
            ImplementationEvaluator::Jrsonnet(jrsonnet) => { jrsonnet.$method($($argument),*)?; }
            ImplementationEvaluator::Reference(reference) => { reference.$method($($argument),*)?; }
        }
    };
}

impl Evaluator {
	/// Build a jrsonnet context initializer with this evaluator's native plugins.
	///
	/// This is for integrations that need a custom jrsonnet import resolver while
	/// retaining the same native functions as the main engine.
	pub fn jrsonnet_context(mut self) -> Result<JrsonnetContext, Error> {
		let options = self.options.clone();
		options.apply(&mut self)?;
		if self.evaluator.is_none() {
			let implementation = self.selected_implementation();
			self.populate_evaluator(implementation)?;
		}
		match self.evaluator.expect("evaluator was populated") {
			ImplementationEvaluator::Jrsonnet(evaluator) => Ok(JrsonnetContext(evaluator)),
			ImplementationEvaluator::Reference(_) => Err(Error::JrsonnetContextOnReference),
		}
	}

	pub fn with_rc(&mut self, rc: Rc) -> Result<&mut Self, Error> {
		self.options.rc.spec.merge_from(rc.spec);
		let implementation = self.selected_implementation();
		self.populate_evaluator(implementation)?;
		call_implementation_evaluator_method!(@no_insert: self, with_rc, self.options.rc.clone());
		Ok(self)
	}

	pub fn with_import_paths(&mut self, import_paths: Vec<PathBuf>) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_import_paths, import_paths);
		Ok(self)
	}

	pub fn with_external_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_external_code, key, value);
		Ok(self)
	}

	pub fn with_native_function<F>(&mut self, key: &str, function: F) -> Result<&mut Self, Error>
	where
		F: 'static + Function<JrsonnetEvaluator>,
	{
		if self.evaluator.is_none() {
			self.populate_evaluator(self.selected_implementation())?;
		}
		match self.evaluator.as_mut().expect("evaluator was populated") {
			ImplementationEvaluator::Jrsonnet(evaluator) => {
				evaluator.with_native_function(key, function)?;
				Ok(self)
			}
			ImplementationEvaluator::Reference(_) => Err(Error::JrsonnetNativeOnReference),
		}
	}

	pub fn with_external_variable(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_external_variable, key, value);
		Ok(self)
	}

	pub fn with_top_level_argument(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_top_level_argument, key, value);
		Ok(self)
	}

	pub fn with_top_level_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_top_level_code, key, value);
		Ok(self)
	}

	fn into_implementation(mut self) -> Result<ImplementationEvaluator, Error> {
		if self.evaluator.is_none() {
			let implementation = self.selected_implementation();
			self.populate_evaluator(implementation)?;
		}
		Ok(self.evaluator.take().expect("evaluator was populated"))
	}

	/// Read owned Serde data while the file's evaluation context is active.
	pub fn deserialize_file<T: serde::de::DeserializeOwned>(
		self,
		path: impl AsRef<Path> + fmt::Debug,
	) -> Result<T, Error> {
		match self.into_implementation()? {
			ImplementationEvaluator::Jrsonnet(evaluator) => {
				Ok(evaluator.evaluate_file_result(path)?.deserialize()?)
			}
			ImplementationEvaluator::Reference(evaluator) => {
				Ok(evaluator.evaluate_file_result(path)?.deserialize()?)
			}
		}
	}

	pub fn evaluate_file<P>(self, path: P) -> Result<Evaluation, Error>
	where
		P: AsRef<Path> + fmt::Debug,
	{
		match self.into_implementation()? {
			ImplementationEvaluator::Jrsonnet(evaluator) => evaluator
				.evaluate_file_result(path)
				.map(Evaluation::jr)
				.map_err(|error| JrsonnetError::Evaluator(error).into()),
			ImplementationEvaluator::Reference(evaluator) => evaluator
				.evaluate_file_result(path)
				.map(Evaluation::reference)
				.map_err(|error| rtk_jsonnet_reference::Error::from(error).into()),
		}
	}

	pub fn evaluate_snippet<S>(mut self, snippet: S) -> Result<Evaluation, Error>
	where
		S: AsRef<str> + fmt::Debug,
	{
		if self.evaluator.is_none() {
			let implementation = self.selected_implementation();
			self.populate_evaluator(implementation)?;
		}
		match self.evaluator.take().expect("evaluator was populated") {
			ImplementationEvaluator::Jrsonnet(evaluator) => evaluator
				.evaluate_snippet_result(snippet)
				.map(Evaluation::jr)
				.map_err(|error| JrsonnetError::Evaluator(error).into()),
			ImplementationEvaluator::Reference(evaluator) => evaluator
				.evaluate_snippet_result(snippet)
				.map(Evaluation::reference)
				.map_err(|error| rtk_jsonnet_reference::Error::from(error).into()),
		}
	}

	fn selected_implementation(&self) -> JsonnetImplementation {
		if let Some(implementation) = &self.options.rc.spec.jsonnet_implementation {
			return implementation.implementation().clone();
		}
		// An environment's binary/jrsonnet preference still uses the existing
		// output emulation, but a C++ preference can now select the real backend.
		if let Some(implementation) = self
			.environment
			.as_ref()
			.and_then(|spec| spec.export_jsonnet_implementation.as_ref())
			.map(rtk_spec::canonical::JsonentImplementationOrConfig::implementation)
			.filter(|implementation| matches!(implementation, JsonnetImplementation::Reference))
		{
			return implementation.clone();
		}
		JsonnetImplementation::default()
	}
}

impl Evaluator {
	fn populate_evaluator(
		&mut self,
		implementation: JsonnetImplementation,
	) -> Result<&mut ImplementationEvaluator, Error> {
		// Creating an evaluator only reads the shared implementation; only the
		// first one has to initialize it. Taking the write lock unconditionally
		// would serialize every environment's evaluator construction — the whole
		// standard library included — across the pool exporting them.
		let mut created = {
			let implementations = self
				.engine
				.implementations
				.read()
				.expect("implementations should not be poisoned");
			implementations.create_evaluator(&implementation)
		};

		if created.is_none() {
			let mut implementations = self
				.engine
				.implementations
				.write()
				.expect("implementations should not be poisoned");
			implementations.maybe_init_implementation(implementation.clone())?;
			created = implementations.create_evaluator(&implementation);
		}

		// Unsupported implementations retain the existing jrsonnet fallback.
		let Some(created) = created else {
			// Not an implementation rtk has of its own, as the warning said.
			return self.populate_evaluator(JsonnetImplementation::Jrsonnet);
		};
		let evaluator = self.evaluator.insert(created);

		// tk drops Tanka's native functions for an environment evaluated by a
		// jrsonnet binary, which knows nothing about them, so an environment that
		// asks for one has to lose them here as well — it may well be written to
		// notice their absence and fall back.
		let native_functions = !self.options.rc.spec.disable_native_functions
			&& !self
				.environment
				.as_ref()
				.is_some_and(EnvironmentSpec::emulates_jrsonnet);

		if native_functions {
			// This native stores implementation values directly and has to use
			// each evaluator's own lazy callback API rather than the generic,
			// serde-based plugin interface.
			match evaluator {
				ImplementationEvaluator::Jrsonnet(jrsonnet) => {
					jrsonnet.with_rtk_memoize()?;
				}
				ImplementationEvaluator::Reference(reference) => {
					reference.with_rtk_memoize()?;
				}
			}
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_native_functions::Plugin::new());
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_regex::Plugin::new());
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, self.engine.helm.clone());
			call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_kustomize::Plugin::new());
		}

		// Applied on every population, not once: `with_rc` populates again, and
		// each population builds a fresh implementation evaluator that knows
		// nothing of what the last one was told.
		if let Some(environment) = &self.environment {
			call_implementation_evaluator_method!(@with: evaluator, with_environment, environment);
		}

		Ok(evaluator)
	}
}

enum ImplementationEvaluator {
	Jrsonnet(JrsonnetEvaluator),
	Reference(ReferenceEvaluator),
}

impl fmt::Debug for ImplementationEvaluator {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Jrsonnet(value) => value.fmt(formatter),
			Self::Reference(_) => formatter.write_str("ReferenceEvaluator"),
		}
	}
}

#[derive(Debug, Default)]
struct Implementations {
	engine: Weak<EngineInternals>,
	jrsonnet: Option<JrsonnetImplementation>,
	reference: Option<ReferenceImplementation>,
	/// Implementations rtk does not have, already warned about and standing in
	/// for jrsonnet. Remembering them is what keeps an export from warning
	/// again, and taking the write lock again, for every evaluator it creates.
	fallbacks: Vec<JsonnetImplementation>,
}

impl Implementations {
	/// Create an evaluator for `implementation`, if that is one rtk implements
	/// and has already initialized.
	fn create_evaluator(
		&self,
		implementation: &JsonnetImplementation,
	) -> Option<ImplementationEvaluator> {
		match implementation {
			JsonnetImplementation::Jrsonnet => self
				.jrsonnet
				.as_ref()
				.map(|jrsonnet| ImplementationEvaluator::Jrsonnet(jrsonnet.create_evaluator())),
			JsonnetImplementation::Reference => self.reference.as_ref().map(|reference| {
				ImplementationEvaluator::Reference(Implementation::create_evaluator(reference))
			}),
			unsupported if self.fallbacks.contains(unsupported) => {
				self.create_evaluator(&JsonnetImplementation::Jrsonnet)
			}
			JsonnetImplementation::GoJsonnet | JsonnetImplementation::Binary(_) => None,
		}
	}

	pub fn maybe_init_implementation(
		&mut self,
		implementation: JsonnetImplementation,
	) -> Result<(), Error> {
		let Some(engine) = Weak::upgrade(&self.engine) else {
			panic!("attempt to use implementations after engine is dropped");
		};

		let ready = match &implementation {
			JsonnetImplementation::Reference => self.reference.is_some(),
			JsonnetImplementation::Jrsonnet => self.jrsonnet.is_some(),
			unsupported => self.fallbacks.contains(unsupported),
		};
		if ready {
			return Ok(());
		}

		match implementation {
			JsonnetImplementation::Reference => {
				self.reference = Some(ReferenceImplementation::new()?);
				Ok(())
			}
			JsonnetImplementation::Jrsonnet => {
				let flags = engine
					.options
					.rc
					.flags::<JrsonnetFlag>()
					.map_err(JrsonnetError::Flag)?;
				self.jrsonnet = Some(JrsonnetImplementation::new(flags)?);
				Ok(())
			}
			JsonnetImplementation::GoJsonnet => {
				tracing::warn!("the `go-jsonnet` implementation is not implemented");
				self.fall_back(implementation)
			}
			JsonnetImplementation::Binary(ref binary) => {
				tracing::warn!(binary = ?binary, "the `binary:*` implementation is not implemented");
				self.fall_back(implementation)
			}
		}
	}

	/// Stand jrsonnet in for an implementation rtk does not have.
	fn fall_back(&mut self, implementation: JsonnetImplementation) -> Result<(), Error> {
		self.maybe_init_implementation(JsonnetImplementation::Jrsonnet)?;
		self.fallbacks.push(implementation);
		Ok(())
	}
}

#[derive(Clone, Debug, Default)]
pub struct Options {
	pub rc: Rc,
	pub helm_cache: bool,

	pub ext_code: FxHashMap<Box<str>, Box<str>>,
	pub ext_variables: FxHashMap<Box<str>, Box<str>>,
	pub top_level_arguments: FxHashMap<Box<str>, Box<str>>,
	pub top_level_code: FxHashMap<Box<str>, Box<str>>,

	/// How deep evaluation may recurse before it is called a runaway.
	///
	/// Overrides whatever the project's configuration asks for, being the more
	/// direct request of the two. Left unset, the configured depth applies, and
	/// failing that a default matching tk's.
	pub max_stack: Option<usize>,
}

impl Options {
	/// These options with the project's own `tkrc.yaml` merged in.
	///
	/// tk uses `tkrc.yaml` only to mark where a project starts and never reads
	/// what is in it. rtk reads it, so a project can name the Jsonnet
	/// implementation to use, how deep evaluation may recurse, whether Tanka's
	/// native functions exist, and the versions it expects of its tools. A
	/// project without one is configured exactly as before.
	///
	/// The more direct request still wins: [`Options::apply`] overrides the
	/// configured stack depth with `--max-stack` when that was given.
	pub fn for_project(&self, jpath: &JPath) -> Result<Options, Error> {
		let Some(path) = jpath.rc.as_deref() else {
			return Ok(self.clone());
		};

		let rc = rtk_spec::canonical::Rc::load(path).map_err(|source| Error::Rc {
			path: path.display().to_string(),
			source,
		})?;

		let mut options = self.clone();
		let requested_implementation = options.rc.spec.jsonnet_implementation.clone();
		options.rc.spec.merge_from(rc.spec);
		// A command-line choice already in options is more specific than a
		// project's default. Keep the project's other settings.
		if let Some(requested_implementation) = requested_implementation {
			options.rc.spec.jsonnet_implementation = Some(requested_implementation);
		}
		options.check_expected_tanka_version(path)?;
		options.check_expected_helm_version()?;
		Ok(options)
	}

	/// Refuse to work with a helm the project did not ask for.
	///
	/// Only a project that named one pays for this: helm has to be run to be
	/// asked its version, so a project that said nothing is never made to wait
	/// for it. Checked here rather than when a chart is first rendered because
	/// the helm plugin is shared by every project an export touches, while this
	/// expectation belongs to one of them.
	///
	/// `expectVersions.helm` names a major version, so that is all that is
	/// compared. rtk has no tk behaviour to match: tk never reads this file.
	fn check_expected_helm_version(&self) -> Result<(), Error> {
		let Some(expected) = self
			.rc
			.spec
			.expect_versions
			.as_ref()
			.and_then(|versions| versions.helm)
		else {
			return Ok(());
		};

		let expected_major = match expected {
			rtk_spec::canonical::HelmVersion::V3 => 3,
			rtk_spec::canonical::HelmVersion::V4 => 4,
		};

		let installed = rtk_jsonnet_helm::installed_helm_major_version().map_err(|reason| {
			Error::UnreadableHelmVersion {
				reason: reason.into_string(),
			}
		})?;

		if installed == expected_major {
			return Ok(());
		}

		Err(Error::UnsatisfiedHelmVersion {
			expected: expected_major,
			installed,
		})
	}

	/// Refuse to work in a project that asked for a different Tanka.
	///
	/// The environment-level `spec.expectVersions.tanka` is checked where tk
	/// checks it, which leaves `eval` and `env list` out. This one is a
	/// statement about the whole project rather than about one environment, and
	/// is checked as soon as the project's configuration is read — so every
	/// command that evaluates anything honours it. There is no tk behaviour to
	/// match: tk never reads this file.
	fn check_expected_tanka_version(&self, path: &Path) -> Result<(), Error> {
		let Some(constraint) = self
			.rc
			.spec
			.expect_versions
			.as_ref()
			.and_then(|versions| versions.tanka.as_deref())
			.filter(|constraint| !constraint.is_empty())
		else {
			return Ok(());
		};

		let constraints = rtk_masterminds::Constraints::parse(constraint).map_err(|source| {
			Error::UnreadableProjectTankaConstraint {
				path: path.display().to_string(),
				reason: source.to_string(),
			}
		})?;

		if constraints.matches(&rtk_masterminds::tanka_version()) {
			return Ok(());
		}

		Err(Error::UnsatisfiedProjectTankaVersion {
			constraint: constraint.to_owned(),
		})
	}

	pub fn apply(&self, evaluator: &mut Evaluator) -> Result<(), Error> {
		let rc = match self.max_stack {
			Some(max_stack) => {
				let mut rc = self.rc.clone();
				rc.spec.max_stack_depth = Some(max_stack);
				rc
			}
			None => self.rc.clone(),
		};

		evaluator.with_rc(rc)?;

		for (top_level_code, value) in &self.ext_code {
			evaluator.with_external_code(top_level_code, value)?;
		}
		for (top_level_args, value) in &self.ext_variables {
			evaluator.with_external_variable(top_level_args, value)?;
		}

		for (top_level_args, value) in &self.top_level_arguments {
			evaluator.with_top_level_argument(top_level_args, value)?;
		}
		for (top_level_code, value) in &self.top_level_code {
			evaluator.with_top_level_code(top_level_code, value)?;
		}

		Ok(())
	}

	pub fn has_top_level_args(&self) -> bool {
		!self.top_level_arguments.is_empty() || !self.top_level_code.is_empty()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Hidden;
	use std::fs;

	#[test]
	fn evaluates_composed_native_plugins() {
		let evaluation = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(
				r#"{
					escaped: std.native("escapeStringRegex")("a.b"),
					json: std.native("manifestJsonFromJson")('{"a":1}', 2),
					matched: std.native("regexMatch")("^a", "abc"),
					parsedJson: std.native("parseJson")('{"a":1}'),
					parsedYaml: std.native("parseYaml")("mode: 0755"),
					sha256: std.native("sha256")("foo"),
					substituted: std.native("regexSubst")("a", "banana", "o"),
					yaml: std.native("manifestYamlFromJson")('{"a":1}'),
				}"#,
			)
			.unwrap();
		assert_eq!(
			serde_json::to_value(evaluation).unwrap(),
			serde_json::json!({
				"escaped": "a\\.b",
				"json": "{\n  \"a\": 1\n}\n",
				"matched": true,
				"parsedJson": {"a": 1},
				"parsedYaml": [{"mode": 493}],
				"sha256": "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
				"substituted": "bonono",
				"yaml": "a: 1\n",
			})
		);
	}

	#[test]
	fn helm_template_validates_release_name_before_loading_chart() {
		let temp = tempfile::tempdir().unwrap();
		let called_from = serde_json::to_string(&temp.path().join("main.jsonnet")).unwrap();
		let invalid = "first argument 'name' is invalid: a lowercase RFC 1123 subdomain";
		let too_long = "first argument 'name' is too long (max: 53 characters)";
		for (name, expected) in [
			("test", "chart path does not exist"),
			("./hello-somewhere", invalid),
			("--hello-somewhere", invalid),
			("this.shouldwork", "chart path does not exist"),
			(&"a".repeat(54), too_long),
			("", invalid),
		] {
			let snippet = format!(
				r#"std.native("helmTemplate")({}, "missing-chart", {{ calledFrom: {called_from} }})"#,
				serde_json::to_string(name).unwrap()
			);
			let error = Engine::new(Options::default())
				.create_evaluator()
				.evaluate_snippet(snippet)
				.unwrap_err();
			assert!(error.to_string().contains(expected), "{name}: {error}");
		}
	}

	#[test]
	fn helm_cache_directory_follows_each_tanka_project_root() {
		let temp = tempfile::tempdir().unwrap();
		let first = temp.path().join("first");
		let second = temp.path().join("second");
		for root in [&first, &second] {
			fs::create_dir_all(root.join("environments/demo")).unwrap();
			fs::write(root.join("tkrc.yaml"), "{}\n").unwrap();
		}

		let first_cache =
			helm_cache_directory(&first.join("environments/demo/main.jsonnet")).unwrap();
		let second_cache =
			helm_cache_directory(&second.join("environments/demo/main.jsonnet")).unwrap();

		assert_eq!(
			first_cache,
			first.canonicalize().unwrap().join("target/helm")
		);
		assert_eq!(
			second_cache,
			second.canonicalize().unwrap().join("target/helm")
		);
		assert_ne!(first_cache, second_cache);
	}

	#[test]
	fn evaluates_rtk_memoize_without_forcing_a_hit() {
		let evaluation = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(
				r#"{
					first: std.native("rtkMemoize")("engine-test", "first"),
					second: std.native("rtkMemoize")(
						value=error "must not evaluate",
						key="engine-test",
					),
				}"#,
			)
			.unwrap();

		assert_eq!(
			serde_json::to_value(evaluation).unwrap(),
			serde_json::json!({ "first": "first", "second": "first" })
		);
	}

	/// Memoizing is for carrying a value from one environment to the next.
	///
	/// Jsonnet already memoizes within a single evaluation, so a cache that did
	/// not outlive one would do nothing at all. A worker exports environments
	/// one after another, and this is the work that saves them.
	#[test]
	fn environments_on_one_thread_share_memoized_values() {
		let engine = Engine::new(Options::default());
		let first = environment_in("first");
		let second = environment_in("second");

		assert_eq!(
			evaluate_as(
				&engine,
				Some(&first),
				r#"std.native("rtkMemoize")("shared-between-environments", "computed once")"#
			),
			serde_json::json!("computed once")
		);

		// The candidate would fail if this environment computed it again.
		assert_eq!(
			evaluate_as(
				&engine,
				Some(&second),
				r#"std.native("rtkMemoize")(
					"shared-between-environments",
					error "must not evaluate",
				)"#
			),
			serde_json::json!("computed once"),
			"the second environment did not reuse what the first had memoized"
		);
	}

	/// Which is why a memoized value must not depend on its environment.
	///
	/// It keeps the external variables of whichever environment computed it, so
	/// one that reads them reports that environment's answer forever after.
	/// Anything that varies per environment belongs in the key instead.
	#[test]
	fn a_memoized_value_keeps_the_environment_that_computed_it() {
		let engine_reading = |answer: &str| {
			let mut options = Options::default();
			options.ext_variables.insert("where".into(), answer.into());
			Engine::new(options)
		};
		let snippet = r#"std.native("rtkMemoize")(
			"environment-dependent",
			{ where: std.extVar("where") },
		)"#;

		assert_eq!(
			evaluate_as(&engine_reading("first"), None, snippet)["where"],
			serde_json::json!("first")
		);
		assert_eq!(
			evaluate_as(&engine_reading("second"), None, snippet)["where"],
			serde_json::json!("first"),
			"a memoized value should carry the environment that computed it"
		);
	}

	/// A cache miss manifests the value before sharing it with other workers, so
	/// imports are resolved by the evaluation that computes the value.
	#[test]
	fn imports_resolve_against_the_evaluation_that_computes_them() {
		let first = tempfile::tempdir().unwrap();
		let second = tempfile::tempdir().unwrap();
		fs::write(first.path().join("shared.libsonnet"), r#""from the first""#).unwrap();
		fs::write(
			second.path().join("shared.libsonnet"),
			r#""from the second""#,
		)
		.unwrap();

		let engine = Engine::new(Options::default());
		let evaluate_from = |directory: &Path, snippet: &str| {
			let mut evaluator = engine.create_evaluator();
			engine.options().apply(&mut evaluator).unwrap();
			evaluator
				.with_import_paths(vec![directory.to_path_buf()])
				.unwrap();
			serde_json::to_value(evaluator.evaluate_snippet(snippet).unwrap()).unwrap()
		};

		assert_eq!(
			evaluate_from(
				first.path(),
				r#"std.native("rtkMemoize")(
					"lazy-import",
					{ imported: import "shared.libsonnet" },
				).imported"#
			),
			serde_json::json!("from the first")
		);

		assert_eq!(
			evaluate_from(
				second.path(),
				r#"std.native("rtkMemoize")(
					"lazy-import",
					error "must not evaluate",
				).imported"#
			),
			serde_json::json!("from the first")
		);
	}

	/// An environment spec that differs from another only in where it deploys.
	fn environment_in(namespace: &str) -> EnvironmentSpec {
		serde_json::from_str(&format!(r#"{{ "namespace": "{namespace}" }}"#))
			.expect("a valid environment spec")
	}

	/// Evaluate `snippet` the way `environment` would be evaluated.
	fn evaluate_as(
		engine: &Engine,
		environment: Option<&EnvironmentSpec>,
		snippet: &str,
	) -> serde_json::Value {
		let mut evaluator = engine.create_evaluator_for(environment);
		engine.options().apply(&mut evaluator).unwrap();
		serde_json::to_value(evaluator.evaluate_snippet(snippet).unwrap()).unwrap()
	}

	/// An environment spec, built the way one is really read.
	fn environment_spec(implementation: Option<&str>) -> EnvironmentSpec {
		let spec = match implementation {
			Some(implementation) => {
				format!(r#"{{ "exportJsonnetImplementation": "{implementation}" }}"#)
			}
			None => "{}".to_owned(),
		};

		serde_json::from_str(&spec).expect("a valid environment spec")
	}

	/// Everything about an evaluation that the requested implementation decides.
	const FORMATTING: &str = r#"{
		floats: std.toString(0.6),
		memoizeMissing: std.native("rtkMemoize") == null,
		nativeFunctionsMissing: std.native("sha256") == null,
		yamlDoc: std.manifestYamlDoc({ a: "b" }, false, false),
		yamlStream: std.manifestYamlStream([]),
	}"#;

	fn formatting_of(engine: &Engine, environment: Option<&EnvironmentSpec>) -> serde_json::Value {
		let mut evaluator = engine.create_evaluator_for(environment);
		engine.options().apply(&mut evaluator).unwrap();
		serde_json::to_value(evaluator.evaluate_snippet(FORMATTING).unwrap()).unwrap()
	}

	#[test]
	fn an_environment_can_ask_to_be_formatted_like_the_jrsonnet_binary() {
		let engine = Engine::new(Options::default());

		// rtk cannot hand an environment over to the implementation it asks for,
		// so it imitates it instead: the same output, from this evaluator. That
		// includes dropping Tanka's native functions, which the implementation
		// being imitated does not have — environments are written to notice.
		assert_eq!(
			formatting_of(
				&engine,
				Some(&environment_spec(Some("binary:/usr/local/bin/jrsonnet")))
			),
			serde_json::json!({
				"floats": "0.6",
				"memoizeMissing": true,
				"nativeFunctionsMissing": true,
				"yamlDoc": "a: b",
				"yamlStream": "...\n",
			})
		);

		// An environment that asks for nothing is formatted the way tk formats it,
		// which is go-jsonnet's way.
		assert_eq!(
			formatting_of(&engine, Some(&environment_spec(None))),
			serde_json::json!({
				"floats": "0.59999999999999998",
				"memoizeMissing": false,
				"nativeFunctionsMissing": false,
				"yamlDoc": "a: \"b\"",
				"yamlStream": "---\n\n...\n",
			})
		);
	}

	#[test]
	fn formatting_does_not_outlast_the_environment_that_asked_for_it() {
		let engine = Engine::new(Options::default());

		// Some of how a value is formatted belongs to the thread doing the
		// formatting, and a thread exporting several environments does them one
		// after another. What one environment asked for must not decide how the
		// next one is written out.
		let jrsonnet = environment_spec(Some("binary:/usr/local/bin/jrsonnet"));
		assert_eq!(formatting_of(&engine, Some(&jrsonnet))["floats"], "0.6");

		let plain = environment_spec(None);
		assert_eq!(
			formatting_of(&engine, Some(&plain))["floats"],
			"0.59999999999999998",
			"the previous environment's formatting leaked into this one"
		);
	}

	/// A project with a `tkrc.yaml`, and a jpath pointing at it.
	fn project(tkrc: Option<&str>) -> (tempfile::TempDir, JPath) {
		let directory = tempfile::tempdir().expect("a temporary directory");
		std::fs::write(directory.path().join("jsonnetfile.json"), "{}").expect("the marker");
		if let Some(contents) = tkrc {
			std::fs::write(directory.path().join("tkrc.yaml"), contents).expect("the tkrc");
		}
		std::fs::write(directory.path().join("main.jsonnet"), "{}").expect("the entrypoint");
		let jpath = JPath::resolve(&directory.path().join("main.jsonnet")).expect("it resolves");
		(directory, jpath)
	}

	/// tk uses `tkrc.yaml` only to mark where a project starts. rtk reads it, so
	/// its settings finally reach evaluation — nothing loaded the file before.
	#[test]
	fn a_projects_configuration_is_read_from_its_tkrc() {
		let (_directory, jpath) = project(Some("spec:\n  maxStackDepth: 42\n"));
		let options = Options::default()
			.for_project(&jpath)
			.expect("the tkrc reads");
		assert_eq!(options.rc.spec.max_stack_depth, Some(42));
	}

	#[test]
	fn a_reference_request_does_not_evaluate_with_jrsonnet() {
		let mut options = Options::default();
		options.rc.spec.jsonnet_implementation = Some(
			rtk_spec::canonical::JsonentImplementationOrConfig::JsonnetImplementation(
				JsonnetImplementation::Reference,
			),
		);
		let result = Engine::new(options)
			.create_evaluator()
			.evaluate_snippet("1");
		match result {
			Ok(evaluation) => assert!(evaluation.is_reference()),
			Err(Error::Reference(
				rtk_jsonnet_reference::Error::Load { .. }
				| rtk_jsonnet_reference::Error::Version { .. },
			)) => (),
			Err(error) => panic!("reference selection failed: {error}"),
		}
	}

	fn reference_engine() -> Option<Engine> {
		ReferenceImplementation::installed_for_tests()?;
		let mut options = Options::default();
		options.rc.spec.jsonnet_implementation = Some(
			rtk_spec::canonical::JsonentImplementationOrConfig::JsonnetImplementation(
				JsonnetImplementation::Reference,
			),
		);
		Some(Engine::new(options))
	}

	#[test]
	fn environment_can_select_reference_without_a_project_choice() {
		if ReferenceImplementation::installed_for_tests().is_none() {
			return;
		}
		let engine = Engine::new(Options::default());
		let environment = environment_spec(Some("c++"));
		let mut evaluator = engine.create_evaluator_for(Some(&environment));
		engine.options().apply(&mut evaluator).unwrap();
		assert!(evaluator.evaluate_snippet("1").unwrap().is_reference());
	}

	#[test]
	fn reference_evaluates_values_and_fails_eagerly() {
		let Some(engine) = reference_engine() else {
			return;
		};
		let value = engine
			.create_evaluator()
			.evaluate_snippet(r#"{ label: "hello", items: [false, 2], hidden:: error "hidden" }"#)
			.unwrap();
		value.with_root(|value| {
			let object = value.as_object().unwrap();
			assert_eq!(object.field_names(Hidden::Skip).len(), 2);
			assert!(object.get("hidden", Hidden::Include).unwrap().is_none());
			assert_eq!(
				object
					.get_or_bail("label", Hidden::Skip)
					.unwrap()
					.as_str()
					.as_deref(),
				Some("hello")
			);
			let items = object
				.get_or_bail("items", Hidden::Skip)
				.unwrap()
				.as_array()
				.unwrap();
			let items: Vec<_> = items
				.iter()
				.map(|v| v.unwrap().manifest().unwrap())
				.collect();
			assert_eq!(items, ["false", "2"]);
		});
		assert!(
			engine
				.create_evaluator()
				.evaluate_snippet(r#"{ ok: 1, bad: error "eager" }"#)
				.is_err()
		);
		assert_eq!(
			engine
				.create_evaluator()
				.evaluate_snippet(r#"std.native("regexMatch")("^a", "abc")"#)
				.unwrap()
				.with_root(|v| v.as_bool()),
			Some(true)
		);
	}

	#[test]
	fn reference_retains_environment_data() {
		let Some(engine) = reference_engine() else {
			return;
		};
		let value = engine
			.create_evaluator()
			.evaluate_snippet(
				r#"{
			apiVersion: "tanka.dev/v1alpha1", kind: "Environment",
			metadata: { name: "demo" }, spec: { namespace: "demo" },
			data: { resources: [{ kind: "ConfigMap", apiVersion: "v1" }] },
		}"#,
			)
			.unwrap();
		let environment: rtk_spec::canonical::Environment<'_> =
			value.with_root(|root| root.deserialize()).unwrap();
		assert_eq!(environment.metadata.name.as_deref(), Some("demo"));
		let data = value.with_root(|root| {
			root.as_object()
				.unwrap()
				.get_or_bail("data", Hidden::Skip)
				.unwrap()
				.retain()
		});
		drop(value);
		data.with_root(|v| {
			assert_eq!(
				v.as_object()
					.unwrap()
					.get_or_bail("resources", Hidden::Skip)
					.unwrap()
					.as_array()
					.unwrap()
					.iter()
					.map(|item| item.unwrap().deserialize::<serde_json::Value>().unwrap())
					.collect::<Vec<_>>(),
				[serde_json::json!({ "kind": "ConfigMap", "apiVersion": "v1" })]
			)
		});
	}

	#[test]
	fn missing_and_null_fields_are_distinct_on_either_implementation() {
		for engine in [Some(Engine::new(Options::default())), reference_engine()]
			.into_iter()
			.flatten()
		{
			engine
				.create_evaluator()
				.evaluate_snippet("{ present: null }")
				.unwrap()
				.with_root(|v| {
					let object = v.as_object().unwrap();
					assert!(object.get("data", Hidden::Skip).unwrap().is_none());
					assert!(
						object
							.get_or_bail("present", Hidden::Skip)
							.unwrap()
							.is_null()
					);
				});
		}
	}

	#[test]
	fn scoped_deserialization_cannot_detach_nested_jrsonnet_values() {
		use rtk_jsonnet_core::RawValue;
		use rtk_jsonnet_jrsonnet::Value;
		use std::collections::BTreeMap;

		let value = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet("{ data: [42] }")
			.unwrap();
		let err = value
			.with_root(|v| v.deserialize::<BTreeMap<String, Vec<RawValue<Value>>>>())
			.err()
			.expect("nested raw value must not escape the scope");
		assert!(err.to_string().contains("cannot detach"), "{err}");
	}

	#[test]
	fn reference_errors_are_reported_once() {
		let Some(engine) = reference_engine() else {
			return;
		};
		let error = engine
			.create_evaluator()
			.evaluate_snippet("error 'boom'")
			.expect_err("an error")
			.to_string();
		assert!(error.starts_with("RUNTIME ERROR: boom"), "{error}");
	}

	/// An implementation rtk does not have is warned about once and then stands
	/// in for jrsonnet, rather than taking the write lock and warning again for
	/// every evaluator an export creates.
	#[test]
	fn an_unsupported_implementation_falls_back_once() {
		let mut options = Options::default();
		options.rc.spec.jsonnet_implementation = Some(
			rtk_spec::canonical::JsonentImplementationOrConfig::JsonnetImplementation(
				JsonnetImplementation::Binary("/usr/local/bin/jsonnet".into()),
			),
		);
		let engine = Engine::new(options);
		for _ in 0..3 {
			let mut evaluator = engine.create_evaluator();
			engine.options().apply(&mut evaluator).unwrap();
			assert!(!evaluator.evaluate_snippet("1").unwrap().is_reference());
		}
		let implementations = engine.0.implementations.read().unwrap();
		assert_eq!(
			implementations.fallbacks,
			[JsonnetImplementation::Binary(
				"/usr/local/bin/jsonnet".into()
			)]
		);
		assert!(
			implementations
				.create_evaluator(&JsonnetImplementation::Binary(
					"/usr/local/bin/jsonnet".into()
				))
				.is_some(),
			"a known fallback is created under the read lock"
		);
		drop(implementations);
	}

	#[test]
	fn reference_evaluates_files_and_inline_environments() {
		let Some(engine) = reference_engine() else {
			return;
		};
		let directory = tempfile::tempdir().unwrap();
		let file = directory.path().join("main.jsonnet");
		fs::write(&file, r"{ answer: 42 }").unwrap();
		let result = engine.create_evaluator().evaluate_file(&file).unwrap();
		assert!(result.is_reference());
		assert_eq!(
			result.with_root(|v| v
				.as_object()
				.unwrap()
				.get_or_bail("answer", Hidden::Skip)
				.unwrap()
				.as_number()),
			Some(42.0)
		);

		let environment = environment_in("inline");
		assert_eq!(
			evaluate_as(&engine, Some(&environment), r#"{ namespace: "inline" }"#),
			serde_json::json!({"namespace": "inline"})
		);
	}

	#[test]
	fn jrsonnet_only_native_rejects_reference() {
		let Some(engine) = reference_engine() else {
			return;
		};
		let mut evaluator = engine.create_evaluator();
		struct JrsonnetOnly;
		impl Function<JrsonnetEvaluator> for JrsonnetOnly {
			fn argv(&self) -> (usize, Option<usize>) {
				(0, None)
			}
			fn call(
				&self,
				_: &JrsonnetEvaluator,
				_: <JrsonnetEvaluator as rtk_jsonnet_core::Evaluator>::Arguments,
			) -> Result<rtk_jsonnet_jrsonnet::Value, JrsonnetEvaluatorError> {
				unreachable!()
			}
		}
		let error = evaluator.with_native_function("custom", JrsonnetOnly);
		assert!(matches!(error, Err(Error::JrsonnetNativeOnReference)));
	}

	#[test]
	fn an_explicit_implementation_beats_the_projects_default() {
		let (_directory, jpath) = project(Some("spec:\n  jsonnetImplementation: jrsonnet\n"));
		let mut options = Options::default();
		options.rc.spec.jsonnet_implementation = Some(
			rtk_spec::canonical::JsonentImplementationOrConfig::JsonnetImplementation(
				JsonnetImplementation::Reference,
			),
		);
		let options = options.for_project(&jpath).expect("project configuration");
		assert!(matches!(
			options
				.rc
				.spec
				.jsonnet_implementation
				.as_ref()
				.map(rtk_spec::canonical::JsonentImplementationOrConfig::implementation),
			Some(JsonnetImplementation::Reference)
		));
	}

	#[test]
	fn a_project_without_a_tkrc_is_configured_as_before() {
		let (_directory, jpath) = project(None);
		assert!(jpath.rc.is_none(), "there is no tkrc to find");
		let options = Options::default()
			.for_project(&jpath)
			.expect("nothing to read");
		assert_eq!(options.rc.spec.max_stack_depth, None);
	}

	/// The flag is the more direct request, so it wins — but only when it was
	/// actually given. It used to carry a default of 500, which was passed on
	/// every run and so beat the project's depth every time, which is why
	/// `maxStackDepth` could never take effect.
	#[test]
	fn a_given_max_stack_beats_the_projects_depth() {
		let (_directory, jpath) = project(Some("spec:\n  maxStackDepth: 42\n"));

		let configured = Options::default()
			.for_project(&jpath)
			.expect("the tkrc reads");
		assert_eq!(configured.rc.spec.max_stack_depth, Some(42));

		let overridden = Options {
			max_stack: Some(900),
			..Options::default()
		}
		.for_project(&jpath)
		.expect("the tkrc reads");
		// `apply` is what resolves the two, and prefers the flag.
		assert_eq!(overridden.max_stack, Some(900));
		assert_eq!(overridden.rc.spec.max_stack_depth, Some(42));
	}

	#[test]
	fn a_project_demanding_another_tanka_is_refused() {
		let (_directory, jpath) =
			project(Some("spec:\n  expectVersions:\n    tanka: \">=0.99.0\"\n"));
		let error = Options::default()
			.for_project(&jpath)
			.expect_err("this Tanka is too old for it");
		assert!(
			error
				.to_string()
				.contains("does not satisfy the version required by the project: '>=0.99.0'"),
			"{error}"
		);
	}

	#[test]
	fn a_project_constraint_masterminds_cannot_read_is_refused() {
		let (_directory, jpath) = project(Some("spec:\n  expectVersions:\n    tanka: nonsense\n"));
		let error = Options::default()
			.for_project(&jpath)
			.expect_err("that is not a constraint");
		assert!(
			error
				.to_string()
				.contains("parsing version constraint: 'improper constraint: nonsense'"),
			"{error}"
		);
	}

	/// Including the `||` alternatives that are why this goes through
	/// `rtk-masterminds` rather than the `semver` crate.
	#[test]
	fn a_project_constraint_this_tanka_satisfies_is_accepted() {
		for constraint in [">=0.30.0", ">= 0.0.0 || < 0.0.0", "0.38.x", "^0.1.2"] {
			let (_directory, jpath) = project(Some(&format!(
				"spec:\n  expectVersions:\n    tanka: \"{constraint}\"\n"
			)));
			Options::default()
				.for_project(&jpath)
				.unwrap_or_else(|error| panic!("{constraint:?} should be satisfied: {error}"));
		}
	}

	/// A project naming a helm it has not got is refused. Skipped where there is
	/// no helm to ask, as the helm golden fixtures are.
	#[test]
	fn a_project_expecting_another_helm_is_refused() {
		let Ok(installed) = rtk_jsonnet_helm::installed_helm_major_version() else {
			return;
		};
		let unwanted = if installed == 3 { 4 } else { 3 };

		let (_directory, jpath) = project(Some(&format!(
			"spec:\n  expectVersions:\n    helm: {unwanted}\n"
		)));
		let error = Options::default()
			.for_project(&jpath)
			.expect_err("that helm is not installed");
		assert!(
			error.to_string().contains(&format!(
				"helm {installed} is installed, but the project expects helm {unwanted}"
			)),
			"{error}"
		);

		// And the helm it does have is accepted.
		let (_directory, jpath) = project(Some(&format!(
			"spec:\n  expectVersions:\n    helm: {installed}\n"
		)));
		Options::default()
			.for_project(&jpath)
			.expect("the installed helm is what was asked for");
	}

	/// A project that named no helm is never made to wait for one. Verified by
	/// hand with `RTK_HELM_PATH` pointing at nothing: the export still runs,
	/// because helm is only asked when an expectation was declared.
	#[test]
	fn a_project_naming_no_helm_does_not_ask_for_one() {
		let (_directory, jpath) = project(Some("spec:\n  maxStackDepth: 42\n"));
		let options = Options::default()
			.for_project(&jpath)
			.expect("no helm expectation to check");
		assert!(
			options
				.rc
				.spec
				.expect_versions
				.as_ref()
				.and_then(|versions| versions.helm)
				.is_none()
		);
	}

	#[test]
	fn a_projects_configuration_overrides_an_environments_preference() {
		let mut options = Options::default();
		options.rc.spec.jsonnet_implementation = Some(
			serde_json::from_str(
				r#"{ "type": "jrsonnet", "flags": { "outputFormat.floats": "go-jsonnet" } }"#,
			)
			.expect("a valid implementation config"),
		);

		let formatting = formatting_of(
			&Engine::new(options),
			Some(&environment_spec(Some("binary:/usr/local/bin/jrsonnet"))),
		);

		// The project named a format outright, so that is the one used, however
		// the environment would have been formatted otherwise.
		assert_eq!(formatting["floats"], "0.59999999999999998");

		// Only what it named, though. The rest still follows the environment.
		assert_eq!(formatting["yamlDoc"], "a: b");
	}

	#[test]
	fn calls_a_top_level_function_even_with_no_arguments_to_pass_it() {
		// Whether an entrypoint has to be called is a question about the
		// entrypoint, not about whether anything was passed to it: one that takes
		// nothing but defaults still has to be called to get an environment out
		// of it.
		let evaluation = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(r#"function(who = "world") { hello: who }"#)
			.unwrap();

		assert_eq!(
			serde_json::to_value(evaluation).unwrap(),
			serde_json::json!({ "hello": "world" })
		);
	}

	#[test]
	fn recurses_as_deeply_as_tk_allows() {
		fn recurses(max_stack: Option<usize>) -> bool {
			let engine = Engine::new(Options {
				max_stack,
				..Options::default()
			});
			let mut evaluator = engine.create_evaluator();
			engine.options().apply(&mut evaluator).unwrap();

			evaluator
				.evaluate_snippet("local f(n) = if n == 0 then 0 else f(n - 1) + 1; f(300)")
				.is_ok()
		}

		// The evaluator's own default is lower than the one tk gives an
		// environment, and environments are written against tk's.
		assert!(recurses(None), "the default depth is shallower than tk's");

		// Asked for a depth outright, that is the depth, in either direction.
		assert!(!recurses(Some(200)));
		assert!(recurses(Some(500)));
	}

	#[test]
	fn honors_top_level_native_function_disable() {
		let mut options = Options::default();
		options.rc.spec.disable_native_functions = true;
		let engine = Engine::new(options);
		for snippet in [
			r#"std.native("sha256")("foo")"#,
			r#"std.native("rtkMemoize")("disabled", 1)"#,
		] {
			assert!(engine.create_evaluator().evaluate_snippet(snippet).is_err());
		}
	}

	#[test]
	fn lazily_traverses_native_function_values_with_their_context() {
		let value = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(
				r#"{
					array: [std.native("sha256")("array")],
					object: { value: std.native("sha256")("object") },
				}"#,
			)
			.unwrap();
		value.with_root(|value| {
			let object = value.as_object().expect("an object");

			let array = object
				.get_or_bail("array", Hidden::Skip)
				.unwrap()
				.as_array()
				.expect("an array");
			let array_value: String = array
				.iter()
				.next()
				.expect("an element")
				.unwrap()
				.deserialize()
				.unwrap();
			assert_eq!(
				array_value,
				"dbe42cc09c16704aa3d60127c60b4e1646fc6da1d4764aa517de053e65a663d7"
			);

			let nested = object
				.get_or_bail("object", Hidden::Skip)
				.unwrap()
				.as_object()
				.expect("an object");
			let object_value: String = nested
				.get_or_bail("value", Hidden::Skip)
				.unwrap()
				.deserialize()
				.unwrap();
			assert_eq!(
				object_value,
				"2958d416d08aa5a472d7b509036cb7eafd542add84527e66a145ea64cb4cdc75"
			);
		});
	}

	#[test]
	fn captured_child_keeps_its_evaluation_after_another_evaluator_runs() {
		let engine = Engine::new(Options::default());
		let evaluate = |dir: &Path, label: &str| {
			fs::write(dir.join("owner.txt"), label).unwrap();
			let mut evaluator = engine.create_evaluator();
			evaluator.with_import_paths(vec![dir.to_owned()]).unwrap();
			evaluator.with_external_variable("owner", label).unwrap();
			evaluator
				.evaluate_snippet(
					r#"{ child: {
						result: std.native("escapeStringRegex")(
							std.extVar("owner") + "." + importstr "owner.txt"
						),
						unused: error "must stay lazy",
						hidden:: error "must stay hidden",
					} }"#,
				)
				.unwrap()
		};
		let child = |root: Evaluation| {
			root.with_root(|v| {
				v.as_object()
					.unwrap()
					.get_or_bail("child", Hidden::Skip)
					.unwrap()
					.retain()
			})
		};
		let result = |child: Evaluation| {
			child.with_root(|v| {
				v.as_object()
					.unwrap()
					.get_or_bail("result", Hidden::Skip)
					.unwrap()
					.deserialize::<String>()
					.unwrap()
			})
		};
		let a = tempfile::tempdir().unwrap();
		let b = tempfile::tempdir().unwrap();
		let retained = child(evaluate(a.path(), "A"));
		assert_eq!(result(child(evaluate(b.path(), "B"))), r"B\.B");
		assert_eq!(result(retained), r"A\.A");
	}

	#[test]
	fn lists_object_field_names_without_forcing_values() {
		let value = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(r#"{ broken: error "forced", item10: 10, item2: 2, tucked:: 3 }"#)
			.unwrap();
		value.with_root(|value| {
			let object = value.as_object().expect("an object");

			let names = |hidden| {
				object
					.field_names(hidden)
					.iter()
					.map(ToString::to_string)
					.collect::<Vec<String>>()
			};

			assert_eq!(names(Hidden::Skip), ["broken", "item10", "item2"]);
			assert_eq!(
				names(Hidden::Include),
				["broken", "item10", "item2", "tucked"]
			);
			assert_eq!(
				object
					.get_or_bail("item2", Hidden::Skip)
					.unwrap()
					.as_number(),
				Some(2.0)
			);
		});
	}

	#[test]
	fn retains_environment_data_lazily_and_manifests_it_later() {
		use rtk_spec::canonical::Environment;

		let value = Engine::new(Options::default())
			.create_evaluator()
			.evaluate_snippet(
				r#"{
					apiVersion: "tanka.dev/v1alpha1",
					kind: "Environment",
					metadata: { name: "environments/demo" },
					spec: { namespace: "demo" },
					data: {
						unused: error "data was forced",
						big: 1e100,
						lazy: { nested: 2 + 3 },
					},
				}"#,
			)
			.unwrap();

		let environment: Environment<'_> = value.with_root(|v| v.deserialize()).unwrap();
		assert_eq!(
			environment.metadata.name.as_deref(),
			Some("environments/demo")
		);

		let data = value.with_root(|v| {
			v.as_object()
				.unwrap()
				.get_or_bail("data", Hidden::Skip)
				.unwrap()
				.retain()
		});
		drop(value);
		data.with_root(|v| {
			let object = v.as_object().expect("an object");
			assert_eq!(
				object
					.get_or_bail("lazy", Hidden::Skip)
					.unwrap()
					.manifest()
					.unwrap(),
				"{\n    \"nested\": 5\n}"
			);
			assert_eq!(
				serde_json::from_str::<serde_json::Value>(
					&object
						.get_or_bail("big", Hidden::Skip)
						.unwrap()
						.manifest()
						.unwrap()
				)
				.unwrap()
				.as_f64(),
				Some(1e100)
			);
		});
	}
}
