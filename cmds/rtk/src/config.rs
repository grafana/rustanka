//! Output formatting settings for the old Jsonnet evaluator.
//!
//! An environment that asks to be evaluated by a jrsonnet binary gets that
//! binary's formatting rather than go-jsonnet's. [`RtkConfig::jrsonnet_defaults`]
//! is what that amounts to, and [`RtkConfig::default`] is go-jsonnet. Nothing is
//! read from disk: these are the only two settings there are.

/// Which formatting the evaluator should imitate.
#[derive(Debug, Clone, Default)]
pub struct RtkConfig {
	/// Output format settings for various Jsonnet functions
	pub output_format: OutputFormatConfig,

	/// When true, disables Tanka-specific native functions (manifestYamlFromJson,
	/// parseYaml, parseJson, etc.). This is useful when tk uses jrsonnet binary
	/// via exportJsonnetImplementation, where these native functions are not available
	/// and the jsonnet code falls back to std.manifestYamlDoc.
	pub disable_tanka_native_functions: bool,
}

/// Output format configuration for Jsonnet evaluation
///
/// Use "jrsonnet" values for environments that use tk with exportJsonnetImplementation
/// pointing to a jrsonnet binary, to match the output format.
#[derive(Debug, Clone, Default)]
pub struct OutputFormatConfig {
	/// Controls float formatting in std.toString and related functions.
	///
	/// - go-jsonnet (default): Use Go's %.17g format (e.g., 0.59999999999999998)
	/// - jrsonnet: Use shortest representation (e.g., 0.6)
	pub floats: Option<JsonnetImplementation>,

	/// Controls the output format for std.manifestYamlDoc.
	///
	/// - go-jsonnet (default): values are always quoted, regardless of quote_keys setting
	/// - jrsonnet: quote_values follows quote_keys (when quote_keys=false, quote_values=false)
	pub std_manifest_yaml_doc: Option<JsonnetImplementation>,

	/// Controls the output format for std.manifestYamlStream with empty arrays.
	///
	/// - go-jsonnet (default): Empty arrays produce "---\n\n" (document marker + empty line)
	/// - jrsonnet: Empty arrays produce "\n" (just a newline)
	pub std_manifest_yaml_stream: Option<JsonnetImplementation>,
}

/// Specifies which jsonnet implementation's behavior to match
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonnetImplementation {
	/// Match go-jsonnet behavior (default)
	#[default]
	GoJsonnet,
	/// Match jrsonnet binary behavior
	Jrsonnet,
}

impl RtkConfig {
	/// Create config with jrsonnet defaults.
	/// Used when spec.exportJsonnetImplementation points to a jrsonnet binary.
	pub fn jrsonnet_defaults() -> Self {
		Self {
			disable_tanka_native_functions: true,
			output_format: OutputFormatConfig {
				floats: Some(JsonnetImplementation::Jrsonnet),
				std_manifest_yaml_doc: Some(JsonnetImplementation::Jrsonnet),
				std_manifest_yaml_stream: Some(JsonnetImplementation::Jrsonnet),
			},
		}
	}
}

/// Check if exportJsonnetImplementation indicates jrsonnet binary usage
///
/// A binary counts when its path *ends with* `jrsonnet`, matching both
/// [`EvaluatorImplementation`](crate::jsonnet::evaluator::EvaluatorImplementation)
/// and `rtk_spec`'s `JsonnetImplementation::emulates_jrsonnet`, so that every
/// command reads a `binary:` path the same way.
pub fn uses_jrsonnet_binary(export_impl: Option<&str>) -> bool {
	export_impl
		.map(|s| {
			// Accept both the raw spec value ("binary:.../jrsonnet") and the
			// parsed EvaluatorImplementation Display form ("jrsonnet")
			s == "jrsonnet" || (s.starts_with("binary:") && s.ends_with("jrsonnet"))
		})
		.unwrap_or(false)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn recognizes_jrsonnet_implementations_by_the_name_of_the_binary() {
		assert!(uses_jrsonnet_binary(Some("jrsonnet")));
		assert!(uses_jrsonnet_binary(Some("binary:/usr/local/bin/jrsonnet")));
		assert!(uses_jrsonnet_binary(Some("binary:/opt/bin/my-jrsonnet")));

		assert!(!uses_jrsonnet_binary(None));
		assert!(!uses_jrsonnet_binary(Some("go-jsonnet")));
		assert!(!uses_jrsonnet_binary(Some(
			"binary:/usr/local/bin/go-jsonnet"
		)));

		// A path that merely mentions jrsonnet is not one, so that this agrees
		// with how EvaluatorImplementation and rtk_spec parse the same value.
		assert!(!uses_jrsonnet_binary(Some(
			"binary:/opt/jrsonnet/bin/go-jsonnet"
		)));
		assert!(!uses_jrsonnet_binary(Some(
			"binary:/usr/local/bin/jrsonnet-0.5.1"
		)));
	}
}
