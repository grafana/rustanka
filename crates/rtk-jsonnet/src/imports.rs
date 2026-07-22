//! imports - Find all transitive imports of a Tanka environment
//!
//! This module provides functionality to discover all files that are
//! transitively imported by a Tanka environment's main.jsonnet file.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::jpath::{self, JPath};
use crate::scan::{PathExt, Source};

#[derive(Debug, Error)]
pub enum Error {
	#[error("failed to resolve {path}")]
	Canonicalize { path: PathBuf, source: io::Error },
	#[error("failed to resolve import paths")]
	JPath(#[from] jpath::Error),
	#[error("failed to read {path}")]
	Read { path: PathBuf, source: io::Error },
}

/// Recursive import scanner. Collects the canonical path of every file that
/// is transitively imported, starting from the environment's entrypoint.
struct Scanner<'a> {
	imports: HashSet<PathBuf>,
	import_paths: &'a [PathBuf],
	root: &'a Path,
}

impl Scanner<'_> {
	/// Scan a file's contents, recording its imports and recursing into any
	/// jsonnet sources among them.
	fn scan(&mut self, file: &Path, content: &str) {
		let current_dir = file.parent_or_root();
		let source = Source::new(content);

		for import in source.imports() {
			let Some(resolved) = self.resolve_import(current_dir, import.path) else {
				continue;
			};
			if self.imports.contains(&resolved) {
				continue;
			}

			let canonical = fs::canonicalize(&resolved).unwrap_or(resolved);
			if !self.imports.insert(canonical.clone()) {
				continue;
			}

			// importstr loads plain text; only jsonnet sources can pull in
			// further imports.
			if import.importstr || !canonical.is_jsonnet_file() {
				continue;
			}
			if let Ok(content) = fs::read_to_string(&canonical) {
				self.scan(&canonical, &content);
			}
		}

		self.collect_chart_files(current_dir, source);
	}

	/// Resolve an import path to an absolute file path, trying the importing
	/// file's directory, the jpath import paths, and the project root.
	fn resolve_import(&self, current_dir: &Path, import: &str) -> Option<PathBuf> {
		let relative = current_dir.join(import);
		if relative.exists() {
			return Some(relative);
		}

		for import_path in self.import_paths {
			let candidate = import_path.join(import);
			if candidate.exists() {
				return Some(candidate);
			}
		}

		// For paths starting with ../, also try stripping and searching from
		// the import paths (Go jsonnet compatibility).
		if import.starts_with("../") {
			let stripped = import.trim_start_matches("../");
			for import_path in self.import_paths {
				let candidate = import_path.join(stripped);
				if candidate.exists() {
					return Some(candidate);
				}
			}
		}

		let root_relative = self.root.join(import);
		root_relative.exists().then_some(root_relative)
	}

	/// Record every relevant file inside chart/kustomize directories the
	/// source references, since they all affect template output.
	fn collect_chart_files(&mut self, current_dir: &Path, source: Source<'_>) {
		for chart_path in source.chart_paths() {
			for chart_dir in chart_path.resolve_dirs(current_dir) {
				let files = walkdir::WalkDir::new(&chart_dir)
					.into_iter()
					.filter_map(Result::ok)
					.filter(|entry| {
						entry.path().is_file() && entry.path().is_chart_relevant_file()
					});
				for file in files {
					self.imports.insert(file.path().canonicalize_or_self());
				}
			}
		}
	}
}

/// Find all transitive imports of an environment at the given path.
///
/// Returns a sorted list of file paths relative to the project root,
/// including the entrypoint file itself. Callers that render the paths are
/// responsible for normalizing separators (`\` to `/` on Windows).
pub fn transitive_imports(dir: impl AsRef<Path>) -> Result<Vec<PathBuf>, Error> {
	let dir = dir.as_ref();
	let dir = fs::canonicalize(dir).map_err(|source| Error::Canonicalize {
		path: dir.to_owned(),
		source,
	})?;

	let jpath = JPath::resolve(&dir)?;
	let content = fs::read_to_string(&jpath.entrypoint).map_err(|source| Error::Read {
		path: jpath.entrypoint.clone(),
		source,
	})?;

	let mut scanner = Scanner {
		imports: HashSet::new(),
		import_paths: &jpath.import_paths,
		root: &jpath.root_directory,
	};
	scanner.scan(&jpath.entrypoint, &content);

	let mut imports = scanner.imports;
	imports.insert(jpath.entrypoint);

	let mut paths: Vec<PathBuf> = imports
		.into_iter()
		.filter_map(|path| {
			path.strip_prefix(&jpath.root_directory)
				.map(Path::to_owned)
				.ok()
		})
		.collect();
	paths.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));

	Ok(paths)
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::*;

	fn test_root() -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/importTree")
	}

	fn expected_tree() -> Vec<PathBuf> {
		[
			"main.jsonnet",
			"trees.jsonnet",
			"trees/apple.jsonnet",
			"trees/cherry.jsonnet",
			"trees/generic.libsonnet",
			"trees/peach.jsonnet",
		]
		.map(PathBuf::from)
		.to_vec()
	}

	#[test]
	fn test_transitive_imports() {
		let result = transitive_imports(test_root()).unwrap();
		assert_eq!(result, expected_tree());
	}

	#[test]
	fn test_transitive_imports_from_entrypoint_file() {
		// Passing the main.jsonnet file directly should work too
		let result = transitive_imports(test_root().join("main.jsonnet")).unwrap();
		assert_eq!(result, expected_tree());
	}

	fn test_root_charts() -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/importTreeCharts")
	}

	#[test]
	fn test_transitive_imports_includes_helm_chart_files() {
		let result = transitive_imports(test_root_charts()).unwrap();

		for path in [
			"charts/my-chart/Chart.yaml",
			"charts/my-chart/values.yaml",
			"charts/my-chart/templates/deployment.yaml",
		] {
			assert!(
				result.contains(&PathBuf::from(path)),
				"should include {path}, got: {result:?}"
			);
		}
	}

	#[test]
	fn test_transitive_imports_includes_kustomize_files() {
		let result = transitive_imports(test_root_charts()).unwrap();

		for path in ["kustomize/kustomization.yaml", "kustomize/deployment.yaml"] {
			assert!(
				result.contains(&PathBuf::from(path)),
				"should include {path}, got: {result:?}"
			);
		}
	}

	#[test]
	fn test_transitive_imports_excludes_non_chart_files() {
		let result = transitive_imports(test_root_charts()).unwrap();

		assert!(
			!result.contains(&PathBuf::from("charts/my-chart/README.md")),
			"should not include README.md, got: {result:?}"
		);
	}

	#[test]
	fn test_transitive_imports_includes_non_yaml_chart_files() {
		// Files like .txt configs embedded in configmaps should be included
		let result = transitive_imports(test_root_charts()).unwrap();

		assert!(
			result.contains(&PathBuf::from("charts/my-chart/config.txt")),
			"should include config.txt, got: {result:?}"
		);
	}
}
