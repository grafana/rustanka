//! Core Jsonnet evaluation for Tanka environments.
//!
//! Handles evaluating Jsonnet files with proper Tanka context, including native
//! functions and environment configuration injection.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use jrsonnet_evaluator::rustc_hash::{FxBuildHasher, FxHashMap};
use jrsonnet_evaluator::stack::set_stack_depth_limit;
use jrsonnet_evaluator::trace::PathResolver;
use jrsonnet_evaluator::{
	set_skip_assertions, tla::TlaArg, FileImportResolver, IStr, ImportResolver, State,
};
use jrsonnet_stdlib::ContextInitializer;

use crate::config::{uses_jrsonnet_binary, JsonnetImplementation, RtkConfig};
use crate::jsonnet::evaluator::{Evaluation, Evaluator, EvaluatorOptions, GlobalEvaluatorOptions};
use crate::jsonnet::jpath;
use crate::spec::Environment;

pub mod builtins;

/// Environment ext code key used by Tanka
const ENV_EXT_CODE_KEY: &str = "tanka.dev/environment";

/// SingleEnvEvalScript returns a single Environment object by name
/// This matches Tanka's SingleEnvEvalScript in pkg/tanka/evaluators.go
/// The %s placeholder is replaced with the environment name
const SINGLE_ENV_EVAL_SCRIPT: &str = r#"
local singleEnv(object) =
  if std.isObject(object)
  then
    if std.objectHas(object, 'apiVersion')
       && std.objectHas(object, 'kind')
    then
      if object.kind == 'Environment'
      && std.member(object.metadata.name, '%s')
      then object
      else {}
    else
      std.mapWithKey(
        function(key, obj)
          singleEnv(obj),
        object
      )
  else if std.isArray(object)
  then
    std.map(
      function(obj)
        singleEnv(obj),
      object
    )
  else {};

singleEnv(main)
"#;

/// Evaluator that uses jrsonnet as the Jsonnet interpreter.
#[derive(Clone, Debug)]
pub struct JrsonnetEvaluator(Arc<GlobalEvaluatorOptions>);

impl Evaluator for JrsonnetEvaluator {
	fn new(options: GlobalEvaluatorOptions) -> Self {
		JrsonnetEvaluator(Arc::new(options))
	}

	fn global_options(&self) -> &GlobalEvaluatorOptions {
		&self.0
	}

	fn collect_cycles(&self) {
		jrsonnet_gcmodule::collect_thread_cycles();
	}

	fn clear_thread_local_state(&self) {
		jrsonnet_evaluator::with_state(|s| s.clear_thread_local_state());
	}

	fn eval_file<P>(&self, path: P, opts: &EvaluatorOptions) -> Result<Evaluation>
	where
		P: AsRef<Path>,
	{
		#[tracing::instrument(skip_all)]
		fn eval_file(
			global: &GlobalEvaluatorOptions,
			path: &Path,
			opts: &EvaluatorOptions,
		) -> Result<Evaluation> {
			let jpath_result = jpath::resolve(path)?;
			let import_resolver = FileImportResolver::new(jpath_result.import_paths.clone());

			let spec = JrsonnetEvaluator::load_spec(&jpath_result)?;

			let state = JrsonnetEvaluator::create_state(
				import_resolver,
				Some(&jpath_result.base),
				spec.as_ref(),
				global,
				opts,
			)?;

			let value = JrsonnetEvaluator::eval_file_inner(
				&state,
				jpath_result.entrypoint.as_ref(),
				opts,
				global,
			)?;

			jrsonnet_gcmodule::collect_thread_cycles();

			let value = serde_json::from_str::<serde_json::Value>(&value)
				.context("failed to parse snippet result as JSON")?;

			Ok(Evaluation { value, spec })
		}

		eval_file(&self.0, path.as_ref(), opts)
	}

	fn eval_snippet<S>(&self, snippet: S, opts: &EvaluatorOptions) -> Result<Evaluation>
	where
		S: AsRef<str>,
	{
		#[tracing::instrument(skip_all)]
		fn eval_snippet(
			global: &GlobalEvaluatorOptions,
			snippet: &str,
			opts: &EvaluatorOptions,
		) -> Result<Evaluation> {
			let import_resolver = FileImportResolver::new(Vec::new());

			let state = JrsonnetEvaluator::create_state(import_resolver, None, None, global, opts)?;

			let value = JrsonnetEvaluator::eval_snippet_inner(&state, snippet, global)?;

			let value = serde_json::from_str::<serde_json::Value>(&value)
				.context("failed to parse snippet result as JSON")?;

			Ok(Evaluation { value, spec: None })
		}

		eval_snippet(&self.0, snippet.as_ref(), opts)
	}

	fn eval_snippet_with_jpath<S>(
		&self,
		snippet: S,
		jpath: Vec<PathBuf>,
		opts: &EvaluatorOptions,
	) -> Result<Evaluation>
	where
		S: AsRef<str>,
	{
		#[tracing::instrument(skip_all)]
		fn eval_snippet_with_jpath(
			global: &GlobalEvaluatorOptions,
			snippet: &str,
			jpath: Vec<PathBuf>,
			opts: &EvaluatorOptions,
		) -> Result<Evaluation> {
			let import_resolver = FileImportResolver::new(jpath);

			let state = JrsonnetEvaluator::create_state(import_resolver, None, None, global, opts)?;

			let value = JrsonnetEvaluator::eval_snippet_inner(&state, snippet, global)?;

			let value = serde_json::from_str::<serde_json::Value>(&value)
				.context("failed to parse snippet result as JSON")?;

			Ok(Evaluation { value, spec: None })
		}

		eval_snippet_with_jpath(&self.0, snippet.as_ref(), jpath, opts)
	}
}

impl JrsonnetEvaluator {
	#[tracing::instrument(skip_all)]
	pub fn eval_snippet_with_import_resolver<S, I>(
		&self,
		snippet: S,
		import_resolver: I,
		opts: &EvaluatorOptions,
	) -> Result<Evaluation>
	where
		S: AsRef<str>,
		I: ImportResolver,
	{
		let state = JrsonnetEvaluator::create_state(import_resolver, None, None, &self.0, opts)?;

		let value = JrsonnetEvaluator::eval_snippet_inner(&state, snippet.as_ref(), &self.0)?;

		let value = serde_json::from_str::<serde_json::Value>(&value)
			.context("failed to parse snippet result as JSON")?;

		Ok(Evaluation { value, spec: None })
	}
}

impl JrsonnetEvaluator {
	/// Apply top-level arguments to a function value
	fn apply_tla(
		val: jrsonnet_evaluator::Val,
		global: &GlobalEvaluatorOptions,
	) -> Result<jrsonnet_evaluator::Val> {
		let mut tla_args: FxHashMap<IStr, TlaArg> = FxHashMap::with_capacity_and_hasher(
			global.tla_code.len() + global.tla_str.len(),
			FxBuildHasher::default(),
		);

		for (key, value) in &global.tla_str {
			tla_args.insert((&**key).into(), TlaArg::String((&**value).into()));
		}
		for (key, value) in &global.tla_code {
			tla_args.insert((&**key).into(), TlaArg::InlineCode((&**value).into()));
		}

		jrsonnet_evaluator::apply_tla(&tla_args, val)
			.map_err(|e| anyhow::anyhow!("TLA application error:\n{}", e))
	}

	/// Apply settings from .rtk-config.yaml to the context initializer
	fn apply_rtk_config(context_init: &ContextInitializer, config: &RtkConfig) {
		use jrsonnet_evaluator::manifest::set_use_go_style_floats;
		use jrsonnet_stdlib::{
			ManifestYamlDocFormatting, ManifestYamlStreamEmptyBehavior,
			ManifestYamlStreamFormatting, QuoteValuesBehavior,
		};

		// Apply std.manifestYamlDoc format setting
		let quote_values_behavior = match config.output_format.std_manifest_yaml_doc {
			Some(JsonnetImplementation::Jrsonnet) => QuoteValuesBehavior::Jrsonnet,
			Some(JsonnetImplementation::GoJsonnet) | None => QuoteValuesBehavior::GoJsonnet,
		};

		let formatting = ManifestYamlDocFormatting {
			quote_values_behavior,
		};
		context_init.set_manifest_yaml_doc_formatting(formatting);

		// Apply std.manifestYamlStream format setting
		let empty_behavior = match config.output_format.std_manifest_yaml_stream {
			Some(JsonnetImplementation::Jrsonnet) => ManifestYamlStreamEmptyBehavior::Jrsonnet,
			Some(JsonnetImplementation::GoJsonnet) | None => {
				ManifestYamlStreamEmptyBehavior::GoJsonnet
			}
		};

		let stream_formatting = ManifestYamlStreamFormatting { empty_behavior };
		context_init.set_manifest_yaml_stream_formatting(stream_formatting);

		// Apply float format setting
		// Default is Go-style (true), set to false for jrsonnet-style
		let use_go_style = match config.output_format.floats {
			Some(JsonnetImplementation::Jrsonnet) => false,
			Some(JsonnetImplementation::GoJsonnet) | None => true,
		};

		set_use_go_style_floats(use_go_style);
	}

	/// Set up the jrsonnet evaluator state with proper configuration
	fn create_state(
		import_resolver: impl ImportResolver,
		config_base: Option<&Path>,
		spec: Option<&Environment>,
		global: &GlobalEvaluatorOptions,
		opts: &EvaluatorOptions,
	) -> Result<State> {
		// Create context initializer with stdlib and native functions
		// Use Absolute resolver so std.thisFile returns absolute paths (like tk does)
		let context_init = ContextInitializer::new(PathResolver::Absolute);

		// Build config: start with defaults based on spec, then merge .rtk-config.yaml if present
		// First check opts.export_jsonnet_implementation (from inline env discovery),
		// then fall back to spec.json (for static environments)
		let export_impl = opts
			.export_jsonnet_implementation
			.as_deref()
			.or_else(|| spec.and_then(|e| e.spec.export_jsonnet_implementation.as_deref()));
		let mut config = if uses_jrsonnet_binary(export_impl) {
			RtkConfig::jrsonnet_defaults()
		} else {
			RtkConfig::default()
		};

		// Load .rtk-config.yaml if present and merge over defaults
		if let Some(base) = config_base {
			if let Some(file_config) = RtkConfig::load_from_directory(base)? {
				config.merge_from(&file_config);
			}
		}

		JrsonnetEvaluator::apply_rtk_config(&context_init, &config);

		// Add external variables from spec (environment config)
		if let Some(env) = spec {
			// Serialize the environment spec as JSON and inject it
			let env_json = serde_json::to_string(env)?;
			context_init
				.add_ext_code(ENV_EXT_CODE_KEY, env_json)
				.map_err(|e| anyhow::anyhow!("failed to add environment ext code:\n{}", e))?;
		}

		// Add user-provided external strings
		for (key, value) in &global.ext_str {
			context_init.add_ext_str((&**key).into(), (&**value).into());
		}

		// Add user-provided external code
		for (key, value) in &global.ext_code {
			context_init
				.add_ext_code(&**key, &**value)
				.map_err(|e| anyhow::anyhow!("failed to add ext code '{}':\n{}", key, e))?;
		}

		// Register native functions for Tanka compatibility (unless disabled)
		if !config.disable_tanka_native_functions {
			JrsonnetEvaluator::register_native_functions(&context_init);
		}

		// Build the state
		let mut builder = State::builder();
		builder
			.import_resolver(import_resolver)
			.context_initializer(context_init);

		jrsonnet_evaluator::stack::set_stack_depth_limit(global.max_stack);

		let state = builder.build();

		Ok(state)
	}

	/// Evaluate the entrypoint file
	fn eval_file_inner(
		state: &State,
		entrypoint: &Path,
		opts: &EvaluatorOptions,
		global: &GlobalEvaluatorOptions,
	) -> Result<String> {
		let _state_guard = state.enter();
		set_skip_assertions(false);

		// For import statements in eval scripts, use just the filename
		// The import resolver will find it in the import paths
		let entrypoint_filename = entrypoint
			.file_name()
			.and_then(|n| n.to_str())
			.ok_or_else(|| anyhow::anyhow!("invalid entrypoint path"))?;

		// For direct imports, use the full path
		let entrypoint_str = entrypoint.to_string_lossy();

		// Determine if we need to apply a filter script
		let result = if let Some(env_name) = &opts.env_name {
			// Use SingleEnvEvalScript to filter to a specific inline environment
			// Use full path so std.thisFile works correctly for helmTemplate/kustomizeBuild
			let eval_script = format!(
				"local main = (import '{}');\n{}",
				entrypoint_str,
				SINGLE_ENV_EVAL_SCRIPT.replace("%s", env_name)
			);
			state
				.evaluate_snippet("<single-env-eval>".to_owned(), &eval_script)
				.map_err(|e| anyhow::anyhow!("evaluation error:\n{}", e))?
		} else if let Some(expr) = &opts.eval_expr {
			// Build an expression that imports the file and applies the eval expression.
			// Match tk's PatternEvalScript: add a dot separator unless expression starts with '['
			let separator = if expr.starts_with('[') { "" } else { "." };
			let eval_script = format!(
				r#"
    local main = (import '{}');
    main{}{}
    "#,
				entrypoint_filename, separator, expr
			);
			state
				.evaluate_snippet("<eval>".to_owned(), &eval_script)
				.map_err(|e| anyhow::anyhow!("evaluation error:\n{}", e))?
		} else {
			// Direct file import
			state
				.import(entrypoint_str.as_ref())
				.map_err(|e| anyhow::anyhow!("evaluation error:\n{}", e))?
		};

		// Apply TLA - always attempt to invoke if result is a function
		// This handles both explicit TLAs and functions with default arguments
		let result = JrsonnetEvaluator::apply_tla(result, global)?;

		// Manifest the result to JSON
		let manifest = result
			.manifest(jrsonnet_evaluator::manifest::JsonFormat::default())
			.map_err(|e| anyhow::anyhow!("manifest error:\n{}", e))?;

		drop(result);
		jrsonnet_gcmodule::collect_thread_cycles();

		Ok(manifest)
	}

	/// Evaluate a Jsonnet snippet
	fn eval_snippet_inner(
		state: &State,
		snippet: &str,
		global: &GlobalEvaluatorOptions,
	) -> Result<String> {
		let _state_guard = state.enter();
		set_skip_assertions(false);

		let result = state
			.evaluate_snippet("<snippet>".to_owned(), snippet)
			.map_err(|e| anyhow::anyhow!("evaluation error:\n{}", e))?;

		let result = JrsonnetEvaluator::apply_tla(result, global)?;

		let manifest = result
			.manifest(jrsonnet_evaluator::manifest::JsonFormat::default())
			.map_err(|e| anyhow::anyhow!("manifest error:\n{}", e))?;

		drop(result);
		jrsonnet_gcmodule::collect_thread_cycles();

		Ok(manifest.to_string())
	}

	/// Load spec.json from the environment directory if it exists.
	/// Also sets metadata.name and metadata.namespace to relative paths matching Go Tanka's behavior.
	fn load_spec(jpath: &jpath::JpathResult) -> Result<Option<Environment>> {
		let spec_path = jpath.base.join("spec.json");
		if !spec_path.exists() {
			return Ok(None);
		}

		let content =
			fs::read_to_string(&spec_path).context(format!("reading {}", spec_path.display()))?;

		let mut env: Environment =
			serde_json::from_str(&content).context(format!("parsing {}", spec_path.display()))?;

		// Set metadata.name to relative path from root to base directory
		// This matches Go Tanka's behavior in pkg/spec/spec.go:ParseDir
		if let Ok(rel_base) = jpath.base.strip_prefix(&jpath.root) {
			env.metadata.name = Some(rel_base.to_string_lossy().to_string());
		}

		// Set metadata.namespace to relative path from root to entrypoint file
		// This matches Go Tanka's behavior in pkg/spec/spec.go:ParseDir
		if let Ok(rel_entrypoint) = jpath.entrypoint.strip_prefix(&jpath.root) {
			env.metadata.namespace = Some(rel_entrypoint.to_string_lossy().to_string());
		}

		Ok(Some(env))
	}

	/// Register Tanka-compatible native functions
	pub(crate) fn register_native_functions(context: &ContextInitializer) {
		use jrsonnet_stdlib::RegexCache;

		use builtins::{
			escape_string_regex, helm_template, kustomize_build, manifest_json_from_json,
			manifest_yaml_from_json, parse_json, parse_yaml, regex_match, regex_subst, rtk_memoize,
			sha256,
		};

		// Core parsing/manifest functions
		context.add_native("parseJson", parse_json {});
		context.add_native("parseYaml", parse_yaml {});
		context.add_native("manifestJsonFromJson", manifest_json_from_json {});
		context.add_native("manifestYamlFromJson", manifest_yaml_from_json {});

		// Hash function
		context.add_native("sha256", sha256 {});

		// Regex functions
		context.add_native("escapeStringRegex", escape_string_regex {});

		// regexMatch and regexSubst need a shared regex cache
		let regex_cache = RegexCache::default();
		context.add_native(
			"regexMatch",
			regex_match {
				cache: regex_cache.clone(),
			},
		);
		context.add_native("regexSubst", regex_subst { cache: regex_cache });

		// Helm and Kustomize
		context.add_native("helmTemplate", helm_template {});
		context.add_native("kustomizeBuild", kustomize_build {});

		// rtk extension: cross-worker global memoization cache
		context.add_native("rtkMemoize", rtk_memoize {});
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tempfile::TempDir;

	use super::*;

	fn default_evaluator() -> JrsonnetEvaluator {
		JrsonnetEvaluator::new(GlobalEvaluatorOptions::default())
	}

	fn evaluator_with(global: GlobalEvaluatorOptions) -> JrsonnetEvaluator {
		JrsonnetEvaluator::new(global)
	}

	fn default_opts() -> EvaluatorOptions {
		EvaluatorOptions::default()
	}

	fn setup_test_env(temp: &TempDir, main_content: &str) -> std::path::PathBuf {
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), r#"{"version": 1}"#).unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), main_content).unwrap();
		root.join("env")
	}

	#[test]
	fn test_eval_simple_object() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ hello: "world", num: 42 }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();

		assert_eq!(result.value["hello"], "world");
		assert_eq!(result.value["num"], 42);
	}

	#[test]
	fn test_eval_with_spec_json() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(&temp, r#"{ data: "test" }"#);

		// Create spec.json
		fs::write(
			env_path.join("spec.json"),
			r#"{
                "apiVersion": "tanka.dev/v1alpha1",
                "kind": "Environment",
                "metadata": { "name": "test-env" },
                "spec": { "namespace": "test-ns" }
            }"#,
		)
		.unwrap();

		let result = default_evaluator()
			.eval_file(env_path.to_str().unwrap(), &EvaluatorOptions::default())
			.unwrap();
		assert!(result.spec.is_some());
		// Note: metadata.name is overridden with the relative path from root to base
		// (matching Go Tanka's behavior in pkg/spec/spec.go:ParseDir)
		// In this test setup, the env is at "env/" relative to root
		assert_eq!(result.spec.unwrap().metadata.name, Some("env".to_string()));
	}

	#[test]
	fn test_eval_with_ext_str() {
		let global = GlobalEvaluatorOptions::builder()
			.ext_str("myvar", "hello")
			.build();

		let result = evaluator_with(global)
			.eval_snippet(r#"{ value: std.extVar("myvar") }"#, &default_opts())
			.unwrap();
		assert_eq!(result.value["value"], "hello");
	}

	#[test]
	fn test_eval_with_ext_code() {
		let global = GlobalEvaluatorOptions::builder()
			.ext_code("mycode", "{ a: 1, b: 2 }")
			.build();

		let result = evaluator_with(global)
			.eval_snippet(r#"{ value: std.extVar("mycode") }"#, &default_opts())
			.unwrap();
		assert_eq!(result.value["value"]["a"], 1);
		assert_eq!(result.value["value"]["b"], 2);
	}

	#[test]
	fn test_eval_native_parse_json() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ parsed: std.native("parseJson")('{"key": "value"}') }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["parsed"]["key"], "value");
	}

	#[test]
	fn test_eval_native_regex_match() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{
                matches: std.native("regexMatch")("^hello.*", "hello world"),
                no_match: std.native("regexMatch")("^foo", "hello world")
            }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["matches"], true);
		assert_eq!(result.value["no_match"], false);
	}

	#[test]
	fn test_eval_import_path_resolution() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		// Create lib directory with a shared libsonnet
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("lib")).unwrap();
		fs::write(root.join("lib/shared.libsonnet"), r#"{ shared: true }"#).unwrap();

		// Create env that imports from lib
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(
			root.join("env/main.jsonnet"),
			r#"local shared = import 'shared.libsonnet'; shared"#,
		)
		.unwrap();

		let result = default_evaluator()
			.eval_file(
				root.join("env").to_str().unwrap(),
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["shared"], true);
	}

	#[test]
	fn test_eval_syntax_error() {
		let result =
			default_evaluator().eval_snippet(r#"{ invalid syntax }"#, &EvaluatorOptions::default());
		assert!(result.is_err());
	}

	#[test]
	fn test_eval_with_tla_str() {
		let global = GlobalEvaluatorOptions::builder()
			.tla_str("name", "World")
			.build();

		let result = evaluator_with(global)
			.eval_snippet(
				r#"function(name) { greeting: "Hello, " + name + "!" }"#,
				&default_opts(),
			)
			.unwrap();
		assert_eq!(result.value["greeting"], "Hello, World!");
	}

	#[test]
	fn test_eval_with_tla_code() {
		let global = GlobalEvaluatorOptions::builder()
			.tla_code("config", r#"{ items: ["a", "b", "c"] }"#)
			.build();

		let result = evaluator_with(global)
			.eval_snippet(
				r#"function(config) { items: config.items, count: std.length(config.items) }"#,
				&default_opts(),
			)
			.unwrap();
		assert_eq!(result.value["count"], 3);
		assert_eq!(result.value["items"][0], "a");
	}

	#[test]
	fn test_eval_with_eval_expr() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{
				data: { nested: { value: 42 } },
				other: "ignored"
			}"#,
		);

		let opts = EvaluatorOptions {
			eval_expr: Some("data.nested".to_string()),
			..Default::default()
		};

		let result = default_evaluator()
			.eval_file(env_path.to_str().unwrap(), &opts)
			.unwrap();
		assert_eq!(result.value["value"], 42);
		assert!(result.value.get("other").is_none());
	}

	#[test]
	fn test_eval_native_sha256() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ hash: std.native("sha256")("hello") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		// SHA256 of "hello" is a known value
		assert_eq!(
			result.value["hash"],
			"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
		);
	}

	#[test]
	fn test_eval_native_regex_subst() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{
				result: std.native("regexSubst")("world", "hello world", "universe")
			}"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"], "hello universe");
	}

	#[test]
	fn test_eval_native_parse_yaml() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ parsed: std.native("parseYaml")("key: value\nnum: 123") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		// parseYaml returns an array of documents
		assert_eq!(result.value["parsed"][0]["key"], "value");
		assert_eq!(result.value["parsed"][0]["num"], 123);
	}

	#[test]
	fn test_eval_local_import() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();

		// Create a local helper file
		fs::write(root.join("env/helper.libsonnet"), r#"{ helper: true }"#).unwrap();

		// Main file imports the local helper
		fs::write(
			root.join("env/main.jsonnet"),
			r#"local h = import './helper.libsonnet'; h"#,
		)
		.unwrap();

		let result = default_evaluator()
			.eval_file(
				root.join("env").to_str().unwrap(),
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["helper"], true);
	}

	#[test]
	fn test_eval_vendor_import() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("vendor/ksonnet-lib")).unwrap();
		fs::write(
			root.join("vendor/ksonnet-lib/ksonnet.libsonnet"),
			r#"{ k: { core: {} } }"#,
		)
		.unwrap();

		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(
			root.join("env/main.jsonnet"),
			r#"local k = import 'ksonnet-lib/ksonnet.libsonnet'; k"#,
		)
		.unwrap();

		let result = default_evaluator()
			.eval_file(
				root.join("env").to_str().unwrap(),
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert!(result.value["k"]["core"].is_object());
	}

	#[test]
	fn test_eval_spec_json_available_as_ext_var() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();

		// Create spec.json with namespace
		fs::write(
			root.join("env/spec.json"),
			r#"{
				"apiVersion": "tanka.dev/v1alpha1",
				"kind": "Environment",
				"metadata": { "name": "test-env" },
				"spec": { "namespace": "my-namespace" }
			}"#,
		)
		.unwrap();

		// Main file accesses the environment ext var
		fs::write(
			root.join("env/main.jsonnet"),
			r#"
			local env = std.extVar("tanka.dev/environment");
			{ namespace: env.spec.namespace }
			"#,
		)
		.unwrap();

		let result = default_evaluator()
			.eval_file(
				root.join("env").to_str().unwrap(),
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["namespace"], "my-namespace");
	}

	#[test]
	fn test_eval_array_output() {
		let result = default_evaluator()
			.eval_snippet(
				r#"[1, 2, 3, "four", { five: 5 }]"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert!(result.value.is_array());
		assert_eq!(result.value[0], 1);
		assert_eq!(result.value[3], "four");
		assert_eq!(result.value[4]["five"], 5);
	}

	#[test]
	fn test_eval_std_library_functions() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{
				upper: std.asciiUpper("hello"),
				lower: std.asciiLower("WORLD"),
				length: std.length([1, 2, 3]),
				join: std.join("-", ["a", "b", "c"]),
			}"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["upper"], "HELLO");
		assert_eq!(result.value["lower"], "world");
		assert_eq!(result.value["length"], 3);
		assert_eq!(result.value["join"], "a-b-c");
	}

	#[test]
	fn test_eval_missing_file() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		// Don't create main.jsonnet

		let result = default_evaluator().eval_file(root.to_str().unwrap(), &default_opts());
		assert!(result.is_err());
	}

	#[test]
	fn test_eval_native_escape_string_regex() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ escaped: std.native("escapeStringRegex")("hello.world*") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["escaped"], r"hello\.world\*");
	}

	// -----------------------------------------------------------------------
	// TLA edge case tests (mirrors Tanka's pkg/tanka/evaluators_test.go)
	// -----------------------------------------------------------------------

	#[test]
	fn test_eval_with_optional_tlas() {
		// Function with default params, no TLAs provided - should use defaults
		// Mirrors Tanka's TestEvalWithOptionalTlas
		let result = default_evaluator()
			.eval_snippet(
				r#"function(foo="bar", baz="baz") { metadata: { name: foo + "-" + baz } }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["metadata"]["name"], "bar-baz");
	}

	#[test]
	fn test_eval_with_optional_tlas_partial_override() {
		// Function with default params, override only one - rest use defaults
		// Mirrors Tanka's TestEvalWithOptionalTlasSpecifiedArg2
		let global = GlobalEvaluatorOptions::builder()
			.tla_code("baz", "'changed'")
			.build();

		let result = evaluator_with(global)
			.eval_snippet(
				r#"function(foo="bar", baz="baz") { metadata: { name: foo + "-" + baz } }"#,
				&default_opts(),
			)
			.unwrap();
		assert_eq!(result.value["metadata"]["name"], "bar-changed");
	}

	#[test]
	fn test_eval_function_zero_params() {
		// Zero-param function should work without TLAs
		// Mirrors Tanka's TestEvalFunctionWithNoTlas
		let result = default_evaluator()
			.eval_snippet(
				r#"function() { metadata: { name: "inline" } }"#,
				&default_opts(),
			)
			.unwrap();
		assert_eq!(result.value["metadata"]["name"], "inline");
	}

	#[test]
	fn test_eval_invalid_tla_arg() {
		// Providing a TLA for a param that doesn't exist should error
		// Mirrors Tanka's TestInvalidTlaArg
		let global = GlobalEvaluatorOptions::builder()
			.tla_code("foo", "'bar'")
			.build();

		let result = evaluator_with(global).eval_snippet(
			r#"function() { metadata: { name: "inline" } }"#,
			&default_opts(),
		);
		assert!(result.is_err(), "should error on invalid TLA arg");
		let err_msg = result.unwrap_err().to_string();
		assert!(
			err_msg.contains("foo"),
			"error should mention the invalid param name, got: {}",
			err_msg
		);
	}

	#[test]
	fn test_eval_tla_with_non_function() {
		// Providing TLAs to a non-function top-level should pass through
		// Mirrors Tanka's TestTlaWithNonFunction
		let global = GlobalEvaluatorOptions::builder()
			.tla_code("foo", "'bar'")
			.build();

		let result = evaluator_with(global).eval_snippet(
			r#"{ apiVersion: "v1", kind: "ConfigMap", metadata: { name: "test" } }"#,
			&default_opts(),
		);
		assert!(result.is_ok(), "TLAs with non-function should not error");
		assert_eq!(result.unwrap().value["kind"], "ConfigMap");
	}

	// -----------------------------------------------------------------------
	// Expression eval tests (mirrors Tanka's TestEvalJsonnetWithExpression)
	// -----------------------------------------------------------------------

	#[test]
	fn test_eval_expression_bracket_syntax() {
		// Expression with bracket notation: ["testCase"]
		// Mirrors Tanka's TestEvalJsonnetWithExpression
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{
				testCase: "object",
				other: "ignored"
			}"#,
		);

		let opts = EvaluatorOptions {
			eval_expr: Some("testCase".to_string()),
			..Default::default()
		};

		let result = default_evaluator()
			.eval_file(env_path.to_str().unwrap(), &opts)
			.unwrap();
		assert_eq!(result.value, "object");
	}

	// -----------------------------------------------------------------------
	// Native function edge case tests (mirrors Tanka's pkg/jsonnet/native/funcs_test.go)
	// -----------------------------------------------------------------------

	#[test]
	fn test_eval_native_parse_json_empty_dict() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("parseJson")("{}") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert!(result.value["result"].is_object());
		assert_eq!(result.value["result"].as_object().unwrap().len(), 0);
	}

	#[test]
	fn test_eval_native_parse_json_key_value() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("parseJson")('{"a": 47}') }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"]["a"], 47);
	}

	#[test]
	fn test_eval_native_parse_yaml_empty() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("parseYaml")("") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		// parseYaml returns an array of documents; empty input should return empty array
		assert!(result.value["result"].is_array());
		assert_eq!(result.value["result"].as_array().unwrap().len(), 0);
	}

	#[test]
	fn test_eval_native_manifest_json_from_json() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("manifestJsonFromJson")("{}", 4) }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"], "{}\n");
	}

	#[test]
	fn test_eval_native_manifest_json_from_json_reindent() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("manifestJsonFromJson")('{ "a": 47}', 4) }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"], "{\n    \"a\": 47\n}\n");
	}

	#[test]
	fn test_eval_native_manifest_yaml_from_json_empty() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("manifestYamlFromJson")("{}") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"], "{}\n");
	}

	#[test]
	fn test_eval_native_manifest_yaml_from_json_key_value() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("manifestYamlFromJson")('{ "a": 47}') }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"], "a: 47\n");
	}

	#[test]
	fn test_eval_native_manifest_yaml_from_json_list() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("manifestYamlFromJson")('{ "list": ["a", "b", "c"]}') }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert!(result.value["result"].as_str().unwrap().contains("- a"));
		assert!(result.value["result"].as_str().unwrap().contains("- b"));
		assert!(result.value["result"].as_str().unwrap().contains("- c"));
	}

	#[test]
	fn test_eval_native_escape_string_regex_empty() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("escapeStringRegex")("") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"], "");
	}

	#[test]
	fn test_eval_native_escape_string_regex_value() {
		// Mirrors Tanka's TestEscapeStringRegexValue
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("escapeStringRegex")("([0-9]+).*\\s") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		// Must match Go's regexp.QuoteMeta output exactly
		assert_eq!(
			result.value["result"].as_str().unwrap(),
			"\\(\\[0-9\\]\\+\\)\\.\\*\\\\s"
		);
	}

	#[test]
	fn test_eval_native_regex_match_no_match() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("regexMatch")("a", "b") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"], false);
	}

	#[test]
	fn test_eval_native_regex_subst_no_change() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("regexSubst")("a", "b", "c") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"], "b");
	}

	#[test]
	fn test_eval_native_regex_subst_valid() {
		let result = default_evaluator()
			.eval_snippet(
				r#"{ result: std.native("regexSubst")("p[^m]*", "pm", "poe") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["result"], "poem");
	}

	#[test]
	fn test_eval_native_sha256_known_value() {
		// Matches Tanka's TestSha256 with "foo" input
		let result = default_evaluator()
			.eval_snippet(
				r#"{ hash: std.native("sha256")("foo") }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(
			result.value["hash"],
			"2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"
		);
	}

	#[test]
	fn test_eval_native_rtk_memoize_basic() {
		// First evaluation of a key computes and returns the value.
		let result = default_evaluator()
			.eval_snippet(
				r#"{ value: std.native("rtkMemoize")("rtk_memoize_basic", { a: 1, b: [2, 3] }) }"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["value"]["a"], 1);
		assert_eq!(result.value["value"]["b"][1], 3);
	}

	#[test]
	fn test_eval_native_rtk_memoize_returns_cached_value() {
		// The second call with the same key returns the first cached value,
		// even though the second thunk would produce a different result. This
		// also proves the second thunk is never evaluated (it would error
		// otherwise).
		let result = default_evaluator()
			.eval_snippet(
				r#"{
					first: std.native("rtkMemoize")("rtk_memoize_cached", "winner"),
					second: std.native("rtkMemoize")("rtk_memoize_cached", error "should not be evaluated"),
				}"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(result.value["first"], "winner");
		assert_eq!(result.value["second"], "winner");
	}

	#[test]
	fn test_eval_native_rtk_memoize_cross_worker() {
		// Many worker threads request the same key concurrently. Exactly one
		// computation wins and all workers observe the same cached value,
		// proving the cache is global and thread-safe.
		use std::thread;

		let evaluator = std::sync::Arc::new(default_evaluator());
		let mut handles = Vec::new();
		for i in 0..16u32 {
			let evaluator = evaluator.clone();
			handles.push(thread::spawn(move || {
				evaluator
					.eval_snippet(
						&format!(r#"std.native("rtkMemoize")("rtk_memoize_cross_worker", {i})"#),
						&EvaluatorOptions::default(),
					)
					.unwrap()
					.value
			}));
		}

		let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
		let winner = results[0].clone();
		assert!(winner.is_number());
		for r in &results {
			assert_eq!(*r, winner, "all workers must observe the same cached value");
		}
	}

	#[test]
	fn test_eval_native_rtk_memoize_failure_allows_retry() {
		// A failed computation must not poison the key: a later call with the
		// same key can recompute successfully.
		let err = default_evaluator().eval_snippet(
			r#"std.native("rtkMemoize")("rtk_memoize_retry", error "boom")"#,
			&EvaluatorOptions::default(),
		);
		assert!(err.is_err());

		let ok = default_evaluator()
			.eval_snippet(
				r#"std.native("rtkMemoize")("rtk_memoize_retry", "recovered")"#,
				&EvaluatorOptions::default(),
			)
			.unwrap();
		assert_eq!(ok.value, "recovered");
	}

	#[test]
	fn test_eval_native_rtk_memoize_reentrant_same_key_errors() {
		// A thunk that memoizes its own key on the same thread must error
		// instead of deadlocking the worker against itself.
		let result = default_evaluator().eval_snippet(
			r#"std.native("rtkMemoize")("rtk_memoize_reentrant", std.native("rtkMemoize")("rtk_memoize_reentrant", 1))"#,
			&EvaluatorOptions::default(),
		);
		assert!(result.is_err());
		let err = result.unwrap_err().to_string();
		assert!(
			err.contains("re-entrant"),
			"error should mention re-entrant evaluation, got: {err}"
		);
	}

	#[test]
	fn test_eval_native_rtk_memoize_rejects_hidden_field() {
		// A top-level hidden field would be dropped by JSON serialization, so
		// memoizing such a value must error instead of silently losing data.
		let result = default_evaluator().eval_snippet(
			r#"std.native("rtkMemoize")("rtk_memoize_hidden", { visible: 1, hidden:: 2 })"#,
			&EvaluatorOptions::default(),
		);
		assert!(result.is_err());
		let err = result.unwrap_err().to_string();
		assert!(
			err.contains("hidden"),
			"error should mention hidden field, got: {err}"
		);
	}

	#[test]
	fn test_eval_native_rtk_memoize_rejects_nested_hidden_field() {
		// Hidden fields are rejected at any depth, including inside arrays.
		let result = default_evaluator().eval_snippet(
			r#"std.native("rtkMemoize")("rtk_memoize_hidden_nested", { a: { b: [{ c:: 1 }] } })"#,
			&EvaluatorOptions::default(),
		);
		assert!(result.is_err());
		let err = result.unwrap_err().to_string();
		assert!(
			err.contains("hidden"),
			"error should mention hidden field, got: {err}"
		);
	}
}
