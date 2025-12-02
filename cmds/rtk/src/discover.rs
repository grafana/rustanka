//! discover - Find Tanka environments in directory trees
//!
//! This module handles discovering all Tanka environments within given paths.
//! An environment is identified by the presence of either:
//! - `spec.json` (static environment)
//! - `main.jsonnet` with inline environment definition

use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Files that indicate a Tanka environment
const ENV_MARKERS: &[&str] = &["spec.json", "main.jsonnet"];

/// Directories to skip during discovery
const SKIP_DIRS: &[&str] = &["vendor", "node_modules", ".git", "lib"];

/// Result of environment discovery
#[derive(Debug)]
pub struct DiscoveredEnv {
	/// Path to the environment directory
	pub path: PathBuf,
	/// Whether this is a static environment (has spec.json)
	#[allow(dead_code)]
	pub is_static: bool,
}

/// Find all Tanka environments in the given paths
///
/// This walks the directory tree looking for environments.
/// When an environment is found, its subdirectories are not searched.
pub fn find_environments(paths: &[String]) -> Result<Vec<DiscoveredEnv>> {
	let mut envs = Vec::new();
	let mut seen_dirs: HashSet<PathBuf> = HashSet::new();

	for path in paths {
		let path = PathBuf::from(path);
		let abs_path = if path.is_absolute() {
			path
		} else {
			std::env::current_dir()?.join(path)
		};

		// If path is directly an environment, add it
		if is_environment(&abs_path) {
			if seen_dirs.insert(abs_path.clone()) {
				envs.push(DiscoveredEnv {
					is_static: abs_path.join("spec.json").exists(),
					path: abs_path,
				});
			}
			continue;
		}

		// Walk the directory tree, filtering out directories we want to skip
		let walker = WalkDir::new(&abs_path)
			.follow_links(true)
			.into_iter()
			.filter_entry(|e| {
				// Only filter directories
				if !e.file_type().is_dir() {
					return true;
				}
				// Skip certain directory names
				if let Some(name) = e.file_name().to_str() {
					if SKIP_DIRS.contains(&name) || name.starts_with('.') {
						return false;
					}
				}
				true
			});

		for entry in walker {
			let entry = match entry {
				Ok(e) => e,
				Err(_) => continue,
			};

			let entry_path = entry.path();

			if entry.file_type().is_dir() && is_environment(entry_path) {
				let canonical = entry_path.to_path_buf();
				if seen_dirs.insert(canonical.clone()) {
					envs.push(DiscoveredEnv {
						is_static: canonical.join("spec.json").exists(),
						path: canonical,
					});
				}
			}
		}
	}

	Ok(envs)
}

/// Check if a directory is a Tanka environment
fn is_environment(path: &Path) -> bool {
	if !path.is_dir() {
		return false;
	}

	// Check for environment markers
	for marker in ENV_MARKERS {
		if path.join(marker).exists() {
			return true;
		}
	}

	false
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	#[test]
	fn test_find_single_environment() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		// Create a single environment
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();

		let envs = find_environments(&[root.join("env").to_string_lossy().to_string()]).unwrap();
		assert_eq!(envs.len(), 1);
		assert!(!envs[0].is_static);
	}

	#[test]
	fn test_find_static_environment() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();
		fs::write(
			root.join("env/spec.json"),
			r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment"}"#,
		)
		.unwrap();

		let envs = find_environments(&[root.join("env").to_string_lossy().to_string()]).unwrap();
		assert_eq!(envs.len(), 1);
		assert!(envs[0].is_static);
	}

	#[test]
	fn test_find_multiple_environments() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create multiple environments
		for name in ["dev", "staging", "prod"] {
			fs::create_dir_all(root.join(format!("environments/{}", name))).unwrap();
			fs::write(
				root.join(format!("environments/{}/main.jsonnet", name)),
				"{}",
			)
			.unwrap();
		}

		let envs =
			find_environments(&[root.join("environments").to_string_lossy().to_string()]).unwrap();
		assert_eq!(envs.len(), 3);
	}

	#[test]
	fn test_skip_vendor_directory() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();

		// Create env in vendor (should be skipped)
		fs::create_dir_all(root.join("vendor/somelib")).unwrap();
		fs::write(root.join("vendor/somelib/main.jsonnet"), "{}").unwrap();

		// Create actual env at root level (not inside environments subdir)
		fs::write(root.join("main.jsonnet"), "{}").unwrap();

		let envs = find_environments(&[root.to_string_lossy().to_string()]).unwrap();
		assert_eq!(envs.len(), 1);
		// Root itself should be the environment
		assert_eq!(envs[0].path, root);
	}

	#[test]
	fn test_no_duplicate_environments() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();

		// Pass the same path twice
		let envs = find_environments(&[
			root.join("env").to_string_lossy().to_string(),
			root.join("env").to_string_lossy().to_string(),
		])
		.unwrap();
		assert_eq!(envs.len(), 1);
	}
}
