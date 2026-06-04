use jrsonnet_evaluator::{IStr, Result, Val, error, function::builtin};
use serde_saphyr::options;

#[builtin]
pub fn builtin_parse_json(str: IStr) -> Result<Val> {
	let value: Val = serde_json::from_str(&str).map_err(|e| error!("failed to parse json: {e}"))?;
	Ok(value)
}

#[builtin]
pub fn builtin_parse_yaml(str: IStr) -> Result<Val> {
	let needs_synthetic_null = str.trim_end().ends_with("\n---");

	let mut out = serde_saphyr::from_multiple_with_options::<Val>(
		&str,
		options! {
			// Golang/C++ compat
			legacy_octal_numbers: true,
			// Disable budget limits - we trust the YAML input
			budget: None,
		},
	)
	.map_err(|e| error!("failed to parse yaml: {e}"))?;

	// saphyr and other yaml implementations disagree on how to handle an empty document in multi-document stream.
	// Saphyr only considers document started after anything is emitted after the document delimiter
	if needs_synthetic_null {
		out.push(Val::Null);
	}

	Ok(if out.is_empty() {
		Val::Null
	} else if out.len() == 1 {
		out.into_iter().next().unwrap()
	} else {
		Val::Arr(out.into())
	})
}
