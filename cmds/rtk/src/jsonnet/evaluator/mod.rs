//! evaluator - Jsonnet evaluation and environment processing for Tanka

#![allow(dead_code)]
#![allow(unused_imports)]

use std::collections::HashMap;
use std::fmt::{self, Formatter};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{anyhow, Context, Error, Result};

use crate::spec::Environment;

pub mod configurable;
pub mod jrsonnet;

pub use configurable::ConfigurableEvaluator as DefaultEvaluator;
pub use configurable::ConfigurableEvaluator;
pub use jrsonnet::JrsonnetEvaluator;

/// Result of jsonnet evaluation
#[derive(Debug)]
pub struct Evaluation {
	/// The evaluated JSON as a serde_json::Value
	pub value: serde_json::Value,
	/// The environment spec (if found) - used by export command
	pub spec: Option<Environment>,
}

#[derive(Clone, Debug, Default)]
pub enum EvaluatorImplementation {
	#[default]
	Jrsonnet,
	/// Accepted for tk compatibility but treated as a no-op — rtk always uses jrsonnet.
	Binary(String),
}

impl fmt::Display for EvaluatorImplementation {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self {
			EvaluatorImplementation::Jrsonnet => formatter.write_str("jrsonnet"),
			EvaluatorImplementation::Binary(path) => write!(formatter, "binary:{path}"),
		}
	}
}

impl FromStr for EvaluatorImplementation {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"jrsonnet" => Ok(Self::Jrsonnet),
			s if s.starts_with("binary:") && s.ends_with("jrsonnet") => {
				tracing::warn!("Treating {s} as the local jrsonnet implementation");
				Ok(Self::Jrsonnet)
			}
			s if s.starts_with("binary:") => Ok(Self::Binary(s["binary:".len()..].to_string())),
			_ => Err(anyhow!(
				"invalid value '{s}': expected 'jrsonnet' or 'binary:<path>'"
			)),
		}
	}
}

/// Global evaluator options derived from CLI arguments.
/// These apply to all file evaluations and are stored in the evaluator at construction time.
#[derive(Debug, Clone)]
pub struct GlobalEvaluatorOptions {
	/// External variables (string values)
	pub ext_str: HashMap<Box<str>, Box<str>>,
	/// External variables (code values)
	pub ext_code: HashMap<Box<str>, Box<str>>,
	/// Top-level arguments (string values)
	pub tla_str: HashMap<Box<str>, Box<str>>,
	/// Top-level arguments (code values)
	pub tla_code: HashMap<Box<str>, Box<str>>,
	/// Maximum stack depth
	pub max_stack: usize,
	/// Which evaluator implementation to use
	pub implementation: EvaluatorImplementation,
}

impl Default for GlobalEvaluatorOptions {
	fn default() -> Self {
		Self {
			ext_str: HashMap::new(),
			ext_code: HashMap::new(),
			tla_str: HashMap::new(),
			tla_code: HashMap::new(),
			max_stack: 500,
			implementation: EvaluatorImplementation::default(),
		}
	}
}

impl GlobalEvaluatorOptions {
	pub fn builder() -> GlobalEvaluatorOptionsBuilder {
		GlobalEvaluatorOptionsBuilder::default()
	}
}

/// Per-evaluation runtime options. These vary per file/snippet evaluation.
#[derive(Debug, Default, Clone)]
pub struct EvaluatorOptions {
	/// Optional eval expression to apply to output (e.g., ".data" or "[0]")
	pub eval_expr: Option<String>,
	/// For inline environments with multiple sub-environments, the name of the specific environment to evaluate
	pub env_name: Option<String>,
	/// exportJsonnetImplementation from the environment spec (discovered from inline env metadata)
	/// Used to determine whether to use jrsonnet-compatible output formatting
	pub export_jsonnet_implementation: Option<String>,
}

/// Builder for [`GlobalEvaluatorOptions`].
#[derive(Debug, Default)]
pub struct GlobalEvaluatorOptionsBuilder {
	ext_str: HashMap<Box<str>, Box<str>>,
	ext_code: HashMap<Box<str>, Box<str>>,
	tla_str: HashMap<Box<str>, Box<str>>,
	tla_code: HashMap<Box<str>, Box<str>>,
	max_stack: Option<usize>,
	implementation: Option<EvaluatorImplementation>,
}

#[allow(dead_code)]
impl GlobalEvaluatorOptionsBuilder {
	pub fn ext_str(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
		self.ext_str.insert(key.into().into(), value.into().into());
		self
	}

	pub fn ext_strs(
		mut self,
		pairs: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
	) -> Self {
		self.ext_str.extend(
			pairs
				.into_iter()
				.map(|(k, v)| (k.into().into(), v.into().into())),
		);
		self
	}

	pub fn ext_code(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
		self.ext_code.insert(key.into().into(), value.into().into());
		self
	}

	pub fn ext_codes(
		mut self,
		pairs: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
	) -> Self {
		self.ext_code.extend(
			pairs
				.into_iter()
				.map(|(k, v)| (k.into().into(), v.into().into())),
		);
		self
	}

	pub fn tla_str(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
		self.tla_str.insert(key.into().into(), value.into().into());
		self
	}

	pub fn tla_strs(
		mut self,
		pairs: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
	) -> Self {
		self.tla_str.extend(
			pairs
				.into_iter()
				.map(|(k, v)| (k.into().into(), v.into().into())),
		);
		self
	}

	pub fn tla_code(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
		self.tla_code.insert(key.into().into(), value.into().into());
		self
	}

	pub fn tla_codes(
		mut self,
		pairs: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
	) -> Self {
		self.tla_code.extend(
			pairs
				.into_iter()
				.map(|(k, v)| (k.into().into(), v.into().into())),
		);
		self
	}

	pub fn max_stack(mut self, max_stack: usize) -> Self {
		self.max_stack = Some(max_stack);
		self
	}

	pub fn implementation(mut self, value: EvaluatorImplementation) -> Self {
		self.implementation = Some(value);
		self
	}

	pub fn build(self) -> GlobalEvaluatorOptions {
		GlobalEvaluatorOptions {
			ext_str: self.ext_str,
			ext_code: self.ext_code,
			tla_str: self.tla_str,
			tla_code: self.tla_code,
			max_stack: self.max_stack.unwrap_or(500),
			implementation: self.implementation.unwrap_or_default(),
		}
	}
}

/// Trait for Jsonnet evaluation of Tanka environments.
pub trait Evaluator: Clone + Send {
	fn new(options: GlobalEvaluatorOptions) -> Self;

	fn global_options(&self) -> &GlobalEvaluatorOptions;

	fn collect_cycles(&self);

	fn clear_thread_local_state(&self);

	fn eval_file<P>(&self, path: P, opts: &EvaluatorOptions) -> Result<Evaluation>
	where
		P: AsRef<Path>;

	fn eval_snippet<S>(&self, snippet: S, opts: &EvaluatorOptions) -> Result<Evaluation>
	where
		S: AsRef<str>;

	fn eval_snippet_with_jpath<S>(
		&self,
		snippet: S,
		jpath: Vec<PathBuf>,
		opts: &EvaluatorOptions,
	) -> Result<Evaluation>
	where
		S: AsRef<str>;

	/// Evaluate an environment path and extract a single environment.
	///
	/// This performs the common pattern of:
	/// 1. Evaluating the Jsonnet
	/// 2. Extracting environments
	/// 3. Setting inline env namespace if needed
	/// 4. Filtering by name if specified
	/// 5. Ensuring exactly one environment is returned
	fn eval_environment(
		&self,
		path: &Path,
		options: &EvaluatorOptions,
		name_filter: Option<&str>,
	) -> Result<crate::spec::EnvironmentData> {
		use crate::environments::{filter_environments_by_name, get_environment_names};

		let mut options = options.clone();
		options.env_name = name_filter.map(|s| s.to_owned());

		tracing::debug!(path = %path.display(), "evaluating environment");
		let eval_result = self
			.eval_file(path, &options)
			.context(format!("evaluating environment at {}", path.display()))?;

		let is_spec_none = eval_result.spec.is_none();
		let mut environments =
			crate::spec::extract_environments(eval_result.value, eval_result.spec);

		if is_spec_none {
			crate::spec::set_inline_env_namespace(&mut environments, path);
		}

		if let Some(target_name) = name_filter {
			// Capture environment names before filtering for use in error message
			let env_names = get_environment_names(&environments);
			environments =
				filter_environments_by_name(environments, target_name).map_err(|_| {
					anyhow::anyhow!(
						"no environment found matching name '{}'. Available environments: {}",
						target_name,
						env_names
					)
				})?;
		}

		let [env_data] = <[_; 1]>::try_from(environments).map_err(|envs: Vec<_>| {
			anyhow::anyhow!(
				"multiple inline environments found ({}). Use --name to select one: {}",
				envs.len(),
				get_environment_names(&envs)
			)
		})?;

		Ok(env_data)
	}
}
