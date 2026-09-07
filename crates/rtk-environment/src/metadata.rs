use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use rtk_jsonnet::jpath::JPath;

/// Fill in the metadata Tanka derives from where an environment lives.
///
/// Mirrors `pkg/spec.ParseDir`: an environment is named after its directory
/// relative to the project root, and its `metadata.namespace` is its entrypoint
/// relative to the project root. Both are overwritten rather than defaulted,
/// because that is what Tanka does — and the values feed the
/// `tanka.dev/environment` label hash and the `--format` template, so they have
/// to agree with it exactly.
///
/// `name` is left alone for inline environments, which are named by the Jsonnet
/// that declares them.
pub(crate) fn apply_paths(metadata: &mut ObjectMeta, jpath: &JPath, name: bool) {
	if name && let Ok(relative) = jpath.base_directory.strip_prefix(&jpath.root_directory) {
		metadata.name = Some(relative_path_name(relative));
	}

	if let Ok(relative) = jpath.entrypoint.strip_prefix(&jpath.root_directory) {
		metadata.namespace = Some(relative_path_name(relative));
	}
}

/// Spell a relative path the way Go's `filepath.Rel` does.
///
/// Stripping a prefix from an equal path leaves nothing, where `filepath.Rel`
/// answers `.` — which is what an environment sitting at its own project root
/// is called in Tanka, and what the `tanka.dev/environment` label hashes.
fn relative_path_name(relative: &std::path::Path) -> String {
	let relative = relative.to_string_lossy();
	if relative.is_empty() {
		".".to_owned()
	} else {
		relative.into_owned()
	}
}
