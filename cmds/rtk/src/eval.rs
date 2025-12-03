//! eval - Jsonnet evaluation for Tanka environments
//!
//! This module handles evaluating jsonnet files with proper tanka context,
//! including native functions and environment configuration injection.

use anyhow::{Context, Result};
use jrsonnet_evaluator::{
	function::TlaArg, gc::GcHashMap, stack::set_stack_depth_limit, trace::PathResolver,
	FileImportResolver, IStr, State,
};
use jrsonnet_stdlib::ContextInitializer;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::jpath::{self, JpathResult};
use crate::spec::Environment;

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

/// Options for evaluation
#[derive(Debug, Default, Clone)]
pub struct EvalOpts {
	/// External variables (string values)
	pub ext_str: HashMap<String, String>,
	/// External variables (code values)
	pub ext_code: HashMap<String, String>,
	/// Top-level arguments (string values)
	pub tla_str: HashMap<String, String>,
	/// Top-level arguments (code values)
	pub tla_code: HashMap<String, String>,
	/// Maximum stack depth
	pub max_stack: Option<usize>,
	/// Optional eval expression to apply to output (e.g., ".data" or "[0]")
	pub eval_expr: Option<String>,
	/// For inline environments with multiple sub-environments, the name of the specific environment to evaluate
	pub env_name: Option<String>,
}

/// Result of jsonnet evaluation
#[derive(Debug)]
pub struct EvalResult {
	/// The evaluated JSON as a serde_json::Value
	pub value: serde_json::Value,
	/// The environment spec (if found) - used by export command
	#[allow(dead_code)]
	pub spec: Option<Environment>,
}

/// Evaluate a tanka environment at the given path
pub fn eval(path: &str, opts: EvalOpts) -> Result<EvalResult> {
	// Resolve jpath (find root, base, import paths)
	let jpath_result = jpath::resolve(path)?;

	// Load spec.json if it exists (static environment)
	// Note: This also sets metadata.name and metadata.namespace to relative paths
	// matching Go Tanka's behavior in pkg/spec/spec.go:ParseDir
	let spec = load_spec(&jpath_result)?;

	// Set up the evaluator state
	let state = setup_state(&jpath_result, &spec, &opts)?;

	// Evaluate the entrypoint
	let result = evaluate_file(&state, &jpath_result.entrypoint, &opts)?;

	// Parse the result as JSON
	let value: serde_json::Value =
		serde_json::from_str(&result).context("failed to parse evaluation result as JSON")?;

	Ok(EvalResult { value, spec })
}

/// Load spec.json from the environment directory if it exists
/// Also sets metadata.name and metadata.namespace to relative paths matching Go Tanka's behavior
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

/// Set up the jrsonnet evaluator state with proper configuration
fn setup_state(jpath: &JpathResult, spec: &Option<Environment>, opts: &EvalOpts) -> Result<State> {
	// Create import resolver with jpath
	let import_resolver = FileImportResolver::new(jpath.import_paths.clone());

	// Create context initializer with stdlib and native functions
	let resolver = PathResolver::new_cwd_fallback();
	let context_init = ContextInitializer::new(resolver);

	// Add external variables from spec (environment config)
	if let Some(env) = spec {
		// Serialize the environment spec as JSON and inject it
		let env_json = serde_json::to_string(env)?;
		context_init
			.add_ext_code(ENV_EXT_CODE_KEY, env_json)
			.map_err(|e| anyhow::anyhow!("failed to add environment ext code: {:?}", e))?;
	}

	// Add user-provided external strings
	for (key, value) in &opts.ext_str {
		context_init.add_ext_str(key.as_str().into(), value.as_str().into());
	}

	// Add user-provided external code
	for (key, value) in &opts.ext_code {
		context_init
			.add_ext_code(key, value.as_str())
			.map_err(|e| anyhow::anyhow!("failed to add ext code '{}': {:?}", key, e))?;
	}

	// Register native functions for Tanka compatibility
	register_native_functions(&context_init);

	// Build the state
	let mut builder = State::builder();
	builder
		.import_resolver(import_resolver)
		.context_initializer(context_init);

	// Set max stack if specified - must be done before building state
	if let Some(max_stack) = opts.max_stack {
		set_stack_depth_limit(max_stack);
	}

	let state = builder.build();

	Ok(state)
}

/// Register Tanka-compatible native functions
fn register_native_functions(context: &ContextInitializer) {
	use jrsonnet_stdlib::{
		builtin_escape_string_regex, builtin_tanka_helm_template, builtin_tanka_kustomize_build,
		builtin_tanka_manifest_json_from_json, builtin_tanka_manifest_yaml_from_json,
		builtin_tanka_parse_json, builtin_tanka_parse_yaml, builtin_tanka_regex_match,
		builtin_tanka_regex_subst, builtin_tanka_sha256, RegexCache,
	};

	// Core parsing/manifest functions
	context.add_native("parseJson", builtin_tanka_parse_json::INST);
	context.add_native("parseYaml", builtin_tanka_parse_yaml::INST);
	context.add_native(
		"manifestJsonFromJson",
		builtin_tanka_manifest_json_from_json::INST,
	);
	context.add_native(
		"manifestYamlFromJson",
		builtin_tanka_manifest_yaml_from_json::INST,
	);

	// Hash function
	context.add_native("sha256", builtin_tanka_sha256::INST);

	// Regex functions
	context.add_native("escapeStringRegex", builtin_escape_string_regex::INST);

	// regexMatch and regexSubst need a shared regex cache
	let regex_cache = RegexCache::default();
	context.add_native(
		"regexMatch",
		builtin_tanka_regex_match {
			cache: regex_cache.clone(),
		},
	);
	context.add_native(
		"regexSubst",
		builtin_tanka_regex_subst { cache: regex_cache },
	);

	// Helm and Kustomize
	context.add_native("helmTemplate", builtin_tanka_helm_template::INST);
	context.add_native("kustomizeBuild", builtin_tanka_kustomize_build::INST);
}

/// Evaluate the entrypoint file
fn evaluate_file(state: &State, entrypoint: &Path, opts: &EvalOpts) -> Result<String> {
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
		let eval_script = format!(
			"local main = (import '{}');\n{}",
			entrypoint_filename,
			SINGLE_ENV_EVAL_SCRIPT.replace("%s", env_name)
		);
		state
			.evaluate_snippet("<single-env-eval>".to_owned(), &eval_script)
			.map_err(|e| anyhow::anyhow!("evaluation error: {:?}", e))?
	} else if let Some(expr) = &opts.eval_expr {
		// Build an expression that imports the file and applies the eval expression
		let eval_script = format!(
			r#"
local main = (import '{}');
main{}
"#,
			entrypoint_filename, expr
		);
		state
			.evaluate_snippet("<eval>".to_owned(), &eval_script)
			.map_err(|e| anyhow::anyhow!("evaluation error: {:?}", e))?
	} else {
		// Direct file import
		state
			.import(entrypoint_str.as_ref())
			.map_err(|e| anyhow::anyhow!("evaluation error: {:?}", e))?
	};

	// Apply TLA if specified
	let result = if !opts.tla_str.is_empty() || !opts.tla_code.is_empty() {
		apply_tla(state, result, opts)?
	} else {
		result
	};

	// Manifest the result to JSON
	let manifest = result
		.manifest(jrsonnet_evaluator::manifest::JsonFormat::default())
		.map_err(|e| anyhow::anyhow!("manifest error: {:?}", e))?;

	Ok(manifest.to_string())
}

/// Apply top-level arguments to a function value
fn apply_tla(
	state: &State,
	val: jrsonnet_evaluator::Val,
	opts: &EvalOpts,
) -> Result<jrsonnet_evaluator::Val> {
	let mut tla_args: GcHashMap<IStr, TlaArg> = GcHashMap::new();

	// Add string TLAs
	for (key, value) in &opts.tla_str {
		tla_args.insert(key.as_str().into(), TlaArg::String(value.as_str().into()));
	}

	// Add code TLAs
	for (key, value) in &opts.tla_code {
		let source = jrsonnet_parser::Source::new_virtual(
			format!("<tla:{}>", key).into(),
			value.as_str().into(),
		);
		let parsed = jrsonnet_parser::parse(
			value,
			&jrsonnet_parser::ParserSettings {
				source: source.clone(),
			},
		)
		.map_err(|e| anyhow::anyhow!("failed to parse TLA code '{}': {:?}", key, e))?;

		tla_args.insert(key.as_str().into(), TlaArg::Code(parsed));
	}

	jrsonnet_evaluator::apply_tla(state.clone(), &tla_args, val)
		.map_err(|e| anyhow::anyhow!("TLA application error: {:?}", e))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	fn setup_test_env(temp: &TempDir, main_content: &str) -> std::path::PathBuf {
		let root = temp.path();
		fs::write(root.join("jsonnetfile.json"), r#"{"version": 1}"#).unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), main_content).unwrap();
		root.join("env")
	}

	#[test]
	fn test_eval_simple_object() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(&temp, r#"{ hello: "world", num: 42 }"#);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();

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

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();
		assert!(result.spec.is_some());
		// Note: metadata.name is overridden with the relative path from root to base
		// (matching Go Tanka's behavior in pkg/spec/spec.go:ParseDir)
		// In this test setup, the env is at "env/" relative to root
		assert_eq!(result.spec.unwrap().metadata.name, Some("env".to_string()));
	}

	#[test]
	fn test_eval_with_ext_str() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(&temp, r#"{ value: std.extVar("myvar") }"#);

		let mut opts = EvalOpts::default();
		opts.ext_str
			.insert("myvar".to_string(), "hello".to_string());

		let result = eval(env_path.to_str().unwrap(), opts).unwrap();
		assert_eq!(result.value["value"], "hello");
	}

	#[test]
	fn test_eval_with_ext_code() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(&temp, r#"{ value: std.extVar("mycode") }"#);

		let mut opts = EvalOpts::default();
		opts.ext_code
			.insert("mycode".to_string(), "{ a: 1, b: 2 }".to_string());

		let result = eval(env_path.to_str().unwrap(), opts).unwrap();
		assert_eq!(result.value["value"]["a"], 1);
		assert_eq!(result.value["value"]["b"], 2);
	}

	#[test]
	fn test_eval_native_parse_json() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{ parsed: std.native("parseJson")('{"key": "value"}') }"#,
		);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();
		assert_eq!(result.value["parsed"]["key"], "value");
	}

	#[test]
	fn test_eval_native_regex_match() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{ 
                matches: std.native("regexMatch")("^hello.*", "hello world"),
                no_match: std.native("regexMatch")("^foo", "hello world")
            }"#,
		);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();
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

		let result = eval(root.join("env").to_str().unwrap(), EvalOpts::default()).unwrap();
		assert_eq!(result.value["shared"], true);
	}

	#[test]
	fn test_eval_syntax_error() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(&temp, r#"{ invalid syntax }"#);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default());
		assert!(result.is_err());
	}

	#[test]
	fn test_eval_with_tla_str() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"function(name) { greeting: "Hello, " + name + "!" }"#,
		);

		let mut opts = EvalOpts::default();
		opts.tla_str.insert("name".to_string(), "World".to_string());

		let result = eval(env_path.to_str().unwrap(), opts).unwrap();
		assert_eq!(result.value["greeting"], "Hello, World!");
	}

	#[test]
	fn test_eval_with_tla_code() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"function(config) { items: config.items, count: std.length(config.items) }"#,
		);

		let mut opts = EvalOpts::default();
		opts.tla_code.insert(
			"config".to_string(),
			r#"{ items: ["a", "b", "c"] }"#.to_string(),
		);

		let result = eval(env_path.to_str().unwrap(), opts).unwrap();
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

		let mut opts = EvalOpts::default();
		opts.eval_expr = Some(".data.nested".to_string());

		let result = eval(env_path.to_str().unwrap(), opts).unwrap();
		assert_eq!(result.value["value"], 42);
		assert!(result.value.get("other").is_none());
	}

	#[test]
	fn test_eval_native_sha256() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(&temp, r#"{ hash: std.native("sha256")("hello") }"#);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();
		// SHA256 of "hello" is a known value
		assert_eq!(
			result.value["hash"],
			"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
		);
	}

	#[test]
	fn test_eval_native_regex_subst() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{ 
				result: std.native("regexSubst")("world", "hello world", "universe")
			}"#,
		);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();
		assert_eq!(result.value["result"], "hello universe");
	}

	#[test]
	fn test_eval_native_parse_yaml() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{ parsed: std.native("parseYaml")("key: value\nnum: 123") }"#,
		);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();
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

		let result = eval(root.join("env").to_str().unwrap(), EvalOpts::default()).unwrap();
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

		let result = eval(root.join("env").to_str().unwrap(), EvalOpts::default()).unwrap();
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

		let result = eval(root.join("env").to_str().unwrap(), EvalOpts::default()).unwrap();
		assert_eq!(result.value["namespace"], "my-namespace");
	}

	#[test]
	fn test_eval_array_output() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(&temp, r#"[1, 2, 3, "four", { five: 5 }]"#);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();
		assert!(result.value.is_array());
		assert_eq!(result.value[0], 1);
		assert_eq!(result.value[3], "four");
		assert_eq!(result.value[4]["five"], 5);
	}

	#[test]
	fn test_eval_std_library_functions() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{
				upper: std.asciiUpper("hello"),
				lower: std.asciiLower("WORLD"),
				length: std.length([1, 2, 3]),
				join: std.join("-", ["a", "b", "c"]),
			}"#,
		);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();
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

		let result = eval(root.to_str().unwrap(), EvalOpts::default());
		assert!(result.is_err());
	}

	#[test]
	fn test_eval_runtime_error() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(&temp, r#"{ value: error "intentional error" }"#);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default());
		assert!(result.is_err());
		assert!(result
			.unwrap_err()
			.to_string()
			.contains("intentional error"));
	}

	#[test]
	fn test_eval_native_escape_string_regex() {
		let temp = TempDir::new().unwrap();
		let env_path = setup_test_env(
			&temp,
			r#"{ escaped: std.native("escapeStringRegex")("hello.world*") }"#,
		);

		let result = eval(env_path.to_str().unwrap(), EvalOpts::default()).unwrap();
		assert_eq!(result.value["escaped"], r"hello\.world\*");
	}
}
