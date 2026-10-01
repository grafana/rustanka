use std::ffi::{CStr, CString, c_char};
use std::path::Path;

use serde::Serialize;

use super::Options;

unsafe extern "C" {
	fn HelmRender(input: *const c_char, error: *mut *mut c_char) -> *mut c_char;
	fn HelmFree(value: *mut c_char);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
	name: &'a str,
	chart: &'a str,
	namespace: &'a Option<String>,
	api_versions: &'a [String],
	include_crds: bool,
	no_hooks: bool,
	values: &'a Option<serde_json::Value>,
}

pub(super) fn render(name: &str, chart: &Path, options: &Options) -> Result<String, String> {
	let chart = chart.to_str().ok_or("chart path is not UTF-8")?;
	let request = Request {
		name,
		chart,
		namespace: &options.namespace,
		api_versions: &options.api_versions,
		include_crds: options.include_crds,
		no_hooks: options.no_hooks,
		values: &options.values,
	};
	let encoded = serde_json::to_string(&request).map_err(|error| error.to_string())?;
	let input = CString::new(encoded).map_err(|error| error.to_string())?;
	// The Go bridge returns owned C strings, including errors, so both paths free them here.
	let mut error = std::ptr::null_mut();
	let output = unsafe { HelmRender(input.as_ptr(), &mut error) };
	if !error.is_null() {
		let message = unsafe { CStr::from_ptr(error) }
			.to_string_lossy()
			.into_owned();
		unsafe { HelmFree(error) };
		return Err(format!("go Helm template failed: {message}"));
	}
	if output.is_null() {
		return Err("Go Helm bridge returned a null response".to_owned());
	}
	let result = unsafe { CStr::from_ptr(output) }.to_bytes().to_owned();
	unsafe { HelmFree(output) };
	String::from_utf8(result).map_err(|error| format!("invalid UTF-8 in Go Helm output: {error}"))
}
