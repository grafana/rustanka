use jrsonnet_evaluator::{function::builtin, runtime_error, IStr, ObjValueBuilder, Result, Val};
use serde::Deserialize;

#[builtin]
pub fn builtin_parse_json(str: IStr) -> Result<Val> {
	let value: Val =
		serde_json::from_str(&str).map_err(|e| runtime_error!("failed to parse json: {e}"))?;
	Ok(value)
}

#[builtin]
pub fn builtin_parse_yaml(str: IStr) -> Result<Val> {
	use serde_yaml_with_quirks::DeserializingQuirks;
	let value = serde_yaml_with_quirks::Deserializer::from_str_with_quirks(
		&str,
		DeserializingQuirks { old_octals: true },
	);
	let mut out = vec![];
	for item in value {
		let val =
			Val::deserialize(item).map_err(|e| runtime_error!("failed to parse yaml: {e}"))?;
		// Expand YAML merge keys (<<) which serde_yaml_with_quirks doesn't handle
		let expanded = expand_merge_keys(val)?;
		out.push(expanded);
	}
	Ok(if out.is_empty() {
		Val::Null
	} else if out.len() == 1 {
		out.into_iter().next().unwrap()
	} else {
		Val::Arr(out.into())
	})
}

/// Recursively expand YAML merge keys (<<) in a Val
/// The merge key << is used in YAML to merge one mapping into another.
/// serde_yaml_with_quirks 0.8.x doesn't expand these, so we need to do it manually.
pub fn expand_merge_keys(val: Val) -> Result<Val> {
	match val {
		Val::Obj(obj) => {
			// Check if this object has a merge key
			let merge_key: IStr = "<<".into();
			let has_merge = obj.get(merge_key.clone())?.is_some();

			if has_merge {
				// Build a new object with merged fields
				let mut builder = ObjValueBuilder::new();

				// First, add fields from the merge target (the value of <<)
				if let Some(merge_val) = obj.get(merge_key.clone())? {
					// The merge value can be an object or an array of objects
					match merge_val {
						Val::Obj(merge_obj) => {
							// Recursively expand the merge target first
							let expanded = expand_merge_keys(Val::Obj(merge_obj))?;
							if let Val::Obj(exp_obj) = expanded {
								for field in exp_obj.fields() {
									if let Some(v) = exp_obj.get(field.clone())? {
										let expanded_v = expand_merge_keys(v)?;
										builder.field(field).try_value(expanded_v)?;
									}
								}
							}
						}
						Val::Arr(arr) => {
							// Array of objects to merge (earlier items have lower priority)
							for item in arr.iter() {
								let item = item?;
								if let Val::Obj(merge_obj) = item {
									let expanded = expand_merge_keys(Val::Obj(merge_obj))?;
									if let Val::Obj(exp_obj) = expanded {
										for field in exp_obj.fields() {
											if let Some(v) = exp_obj.get(field.clone())? {
												let expanded_v = expand_merge_keys(v)?;
												builder.field(field).try_value(expanded_v)?;
											}
										}
									}
								}
							}
						}
						_ => {
							// Invalid merge value, just skip it
						}
					}
				}

				// Then add/override with fields from the original object (except <<)
				for field in obj.fields() {
					if field.as_str() != "<<" {
						if let Some(v) = obj.get(field.clone())? {
							let expanded_v = expand_merge_keys(v)?;
							builder.field(field).try_value(expanded_v)?;
						}
					}
				}

				Ok(Val::Obj(builder.build()))
			} else {
				// No merge key, but still need to recursively process values
				let mut builder = ObjValueBuilder::new();
				for field in obj.fields() {
					if let Some(v) = obj.get(field.clone())? {
						let expanded_v = expand_merge_keys(v)?;
						builder.field(field).try_value(expanded_v)?;
					}
				}
				Ok(Val::Obj(builder.build()))
			}
		}
		Val::Arr(arr) => {
			// Recursively process array elements
			let mut out = Vec::with_capacity(arr.len());
			for item in arr.iter() {
				let item = item?;
				out.push(expand_merge_keys(item)?);
			}
			Ok(Val::Arr(out.into()))
		}
		// Other types don't need processing
		other => Ok(other),
	}
}
