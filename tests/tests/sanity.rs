use jrsonnet_evaluator::{
	FileImportResolver, Result, State, Val, bail,
	trace::{CompactFormat, PathResolver, TraceFormat},
};
use jrsonnet_stdlib::ContextInitializer;

mod common;

#[test]
fn assert_positive() -> Result<()> {
	let mut s = State::builder();
	s.context_initializer(ContextInitializer::new(PathResolver::new_cwd_fallback()))
		.import_resolver(FileImportResolver::default());
	let s = s.build();

	let v = s.evaluate_snippet("snip".to_owned(), "assert 1 == 1: 'fail'; null")?;
	ensure_val_eq!(v, Val::Null);
	let v = s.evaluate_snippet("snip".to_owned(), "std.assertEqual(1, 1)")?;
	ensure_val_eq!(v, Val::Bool(true));

	Ok(())
}

#[test]
fn assert_negative() -> Result<()> {
	let mut s = State::builder();
	s.context_initializer(ContextInitializer::new(PathResolver::new_cwd_fallback()))
		.import_resolver(FileImportResolver::default());
	let s = s.build();

	let trace_format = CompactFormat::default();

	{
		let Err(e) = s.evaluate_snippet("snip".to_owned(), "assert 1 == 2: 'fail'; null") else {
			bail!("assertion should fail");
		};
		let e = trace_format.format(&e).unwrap();
		ensure!(e.starts_with("assert failed: fail\n"));
	}
	{
		let Err(e) = s.evaluate_snippet("snip".to_owned(), "std.assertEqual(1, 2)") else {
			bail!("assertion should fail")
		};
		let e = trace_format.format(&e).unwrap();
		ensure!(e.starts_with("runtime error: assertion failed: A != B\nA: 1\nB: 2\n"));
	}

	Ok(())
}

/// Regression test: context trimming must not break local variable access
/// in functions called through deep object composition.
#[test]
fn context_trimming_local_in_composed_object() -> Result<()> {
	let mut s = State::builder();
	s.context_initializer(ContextInitializer::new(PathResolver::new_cwd_fallback()))
		.import_resolver(FileImportResolver::default());
	let s = s.build();

	let code = r#"
local base = {
  local isEnabled = true,
  local overrideSuperIfExists(name) =
    local override = if isEnabled then 'enabled' else 'disabled';
    override,
  field_a: overrideSuperIfExists('field_a'),
};
local overlay1 = { extra1: 'a' };
local overlay2 = { extra2: 'b' };
local overlay3 = { extra3: 'c' };
local composed = base + overlay1 + overlay2 + overlay3;
composed.field_a
"#;

	let v = s.evaluate_snippet("snip".to_owned(), code)?;
	ensure_val_eq!(v, Val::string("enabled"));
	Ok(())
}

/// Regression test: function passed as lazy argument retains its closure context.
#[test]
fn context_trimming_function_as_argument() -> Result<()> {
	let mut s = State::builder();
	s.context_initializer(ContextInitializer::new(PathResolver::new_cwd_fallback()))
		.import_resolver(FileImportResolver::default());
	let s = s.build();

	let code = r#"
local secret = 42;
local myFunc() = secret;
local apply(f) = f();
apply(myFunc)
"#;

	let v = s.evaluate_snippet("snip".to_owned(), code)?;
	ensure_val_eq!(v, Val::Num(42.0.try_into().unwrap()));
	Ok(())
}

/// Regression test: lazy argument with local expression referencing outer scope.
#[test]
fn context_trimming_lazy_arg_with_local() -> Result<()> {
	let mut s = State::builder();
	s.context_initializer(ContextInitializer::new(PathResolver::new_cwd_fallback()))
		.import_resolver(FileImportResolver::default());
	let s = s.build();

	let code = r#"
local x = 10;
local f(a) = a;
f(local y = x; y + 1)
"#;

	let v = s.evaluate_snippet("snip".to_owned(), code)?;
	ensure_val_eq!(v, Val::Num(11.0.try_into().unwrap()));
	Ok(())
}

/// Regression test: UsedVars must include var from single-child transparent nodes
/// like UnaryOp(Not, Var("x")). Previously, `!x` had empty UsedVars because
/// Var("x") stores the name in analysis.var (not used_vars), and UnaryOp only
/// copied used_vars.
#[test]
fn context_trimming_unary_op_var() -> Result<()> {
	let mut s = State::builder();
	s.context_initializer(ContextInitializer::new(PathResolver::new_cwd_fallback()))
		.import_resolver(FileImportResolver::default());
	let s = s.build();

	let code = r#"
local clustering = true;
local f(current_node_pods=true) = current_node_pods;
f(current_node_pods=!clustering)
"#;

	let v = s.evaluate_snippet("snip".to_owned(), code)?;
	ensure_val_eq!(v, Val::Bool(false));
	Ok(())
}
