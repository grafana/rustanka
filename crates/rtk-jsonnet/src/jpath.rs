use std::borrow::Cow;
use std::env;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
	#[error(transparent)]
	Io(#[from] io::Error),
	#[error("{0} is not a valid path")]
	InvalidPath(PathBuf),
	#[error(
		"could not find project root directory (no tkrc.yaml or jsonnetfile.json found in the parent directories of {path})"
	)]
	CouldNotFindRoot { path: PathBuf },
	#[error(
		"could not find environment base directory (no {entrypoint} found between {path} and {root_directory})"
	)]
	CouldNotFindBaseDirectory {
		path: PathBuf,
		root_directory: PathBuf,
		entrypoint: PathBuf,
	},
}

#[derive(Debug)]
pub struct JPath {
	/// The project root directory (contains jsonnetfile.json or tkrc.yaml)
	pub root_directory: PathBuf,
	/// The environment base directory (contains the entrypoint)
	pub base_directory: PathBuf,
	/// The tkrc file path (absolute)
	pub rc: Option<PathBuf>,
	/// The entrypoint file path (absolute)
	pub entrypoint: PathBuf,
	/// Import paths for jsonnet evaluation, in order of prescedence.
	pub import_paths: Vec<PathBuf>,
}

impl JPath {
	/// Default entrypoint filename for environments
	pub const DEFAULT_ENTRYPOINT: &str = "main.jsonnet";

	/// Files that indicate a project root (in order of precedence)
	const ROOT_MARKERS: &[&str] = &["tkrc.yaml", "tkrc.yml", "jsonnetfile.json"];

	/// Resolve jpath for the given path (file or directory)
	///
	/// This finds:
	/// - Project root directory: directory containing tkrc.yaml or jsonnetfile.json
	/// - Environment base directory: directory containing main.jsonnet
	/// - Import paths: [base, lib, base/vendor, root/vendor]
	pub fn resolve<P>(path: P) -> Result<JPath, Error>
	where
		P: AsRef<Path>,
	{
		let abs_path = JPath::make_absolute(Cow::Borrowed(path.as_ref()))?;

		let (root_directory, rc) = JPath::find_root_directory_and_rc(&abs_path)?;
		let base_directory = JPath::find_base_directory(&abs_path, &root_directory)?;

		let entrypoint = JPath::get_entrypoint(&abs_path)?;
		let entrypoint = base_directory.join(entrypoint);

		let import_paths = vec![
			base_directory.clone(),
			root_directory.join("lib"),
			base_directory.join("vendor"),
			root_directory.join("vendor"),
		];

		Ok(JPath {
			root_directory,
			base_directory,
			rc,
			entrypoint,
			import_paths,
		})
	}

	/// Find the outermost project root containing a Jsonnet project marker.
	pub fn project_root<P>(path: P) -> Result<PathBuf, Error>
	where
		P: AsRef<Path>,
	{
		Self::find_root_directory_and_rc(path.as_ref()).map(|(root, _)| root)
	}

	/// Find the environment base directory by looking for the entrypoint.
	fn find_base_directory(path: &Path, root_directory: &Path) -> Result<PathBuf, Error> {
		let abs_path = JPath::find_close_directory(Cow::Borrowed(path))?.into_owned();
		let entrypoint = JPath::get_entrypoint(path)?;

		if let Some(base_directory) = JPath::find_outermost_directory_with_file_bounded(
			abs_path.clone(),
			root_directory,
			entrypoint,
		) {
			Ok(base_directory)
		} else {
			Err(Error::CouldNotFindBaseDirectory {
				entrypoint: entrypoint.to_owned(),
				path: abs_path,
				root_directory: root_directory.to_owned(),
			})
		}
	}

	/// Get a "close" directory- this is defined as:
	/// - If `path` is a directory, return that as a pathbuf.
	/// - If `path` is a file, return the parent directory.
	/// - If `path` is a directory that hasn't been created yet, return that
	///   directory.
	/// - If `path` is a file that hasn't been created yet, return the parent
	///   directory of that file.
	fn find_close_directory(path: Cow<'_, Path>) -> Result<Cow<'_, Path>, Error> {
		let abs_path = JPath::make_absolute(path)?;

		// If the `path` doesn't exist yet, guess whether it's a directory based
		// on the existince of an extension.
		if !abs_path.exists() {
			if abs_path.extension().is_some() {
				if let Some(parent) = abs_path.parent() {
					return Ok(match abs_path {
						Cow::Borrowed(_) => Cow::Owned(parent.to_owned()),
						Cow::Owned(mut owned) => {
							owned.pop();
							Cow::Owned(owned)
						}
					});
				} else {
					return Err(Error::InvalidPath(abs_path.into_owned()));
				}
			}
			return Ok(abs_path);
		}

		if abs_path.is_dir() {
			Ok(abs_path)
		} else {
			if let Some(parent) = abs_path.parent() {
				return Ok(match abs_path {
					Cow::Borrowed(_) => Cow::Owned(parent.to_owned()),
					Cow::Owned(mut owned) => {
						owned.pop();
						Cow::Owned(owned)
					}
				});
			} else {
				return Err(Error::InvalidPath(abs_path.into_owned()));
			}
		}
	}

	/// Find the project root directory by looking for marker files.
	/// If a tkrc is found in the
	fn find_root_directory_and_rc(path: &Path) -> Result<(PathBuf, Option<PathBuf>), Error> {
		let abs_path = JPath::find_close_directory(Cow::Borrowed(path))?;

		for marker in JPath::ROOT_MARKERS {
			// abs_path is cloned here in order to be used as a buffer while
			// searching for the outermost directory.
			let abs_path = abs_path.clone().into_owned();
			if let Some(root_directory) =
				JPath::find_outermost_directory_with_file(abs_path, marker.as_ref())
			{
				if marker.starts_with("tkrc") {
					let rc = Some(root_directory.join(marker));
					return Ok((root_directory, rc));
				} else {
					return Ok((root_directory, None));
				}
			}
		}

		Err(Error::CouldNotFindRoot {
			path: abs_path.into_owned(),
		})
	}

	/// Find the outermost parent directory containing the specified file.
	fn find_outermost_directory_with_file(path: PathBuf, file: &Path) -> Option<PathBuf> {
		let mut current = path;
		let mut outermost = None;
		loop {
			current.push(file);
			if current.exists() {
				current.pop();
				outermost = Some(current.clone());
			} else {
				current.pop();
			}
			if !current.pop() {
				return outermost;
			}
		}
	}

	/// Find the outermost parent directory containing the specified file, bounded
	/// by a root directory.
	fn find_outermost_directory_with_file_bounded(
		path: PathBuf,
		root: &Path,
		file: &Path,
	) -> Option<PathBuf> {
		let mut current = path;
		let mut outermost = None;
		loop {
			current.push(file);
			if current.exists() {
				current.pop();
				outermost = Some(current.clone());
			} else {
				current.pop();
			}
			// Ascend unless the root (checked above, inclusively) stopped the walk.
			if current == root || !current.pop() {
				return outermost;
			}
		}
	}

	/// Get the entrypoint from `path`.
	fn get_entrypoint(path: &Path) -> Result<&Path, Error> {
		if path.is_dir() {
			return Ok(JPath::DEFAULT_ENTRYPOINT.as_ref());
		}

		if let Some(entrypoint) = path.file_name() {
			Ok(entrypoint.as_ref())
		} else {
			Err(Error::InvalidPath(path.to_owned()))
		}
	}

	/// Takes in `path` and makes it absolute.
	fn make_absolute(path: Cow<'_, Path>) -> Result<Cow<'_, Path>, Error> {
		let path_ref = path.as_ref();
		if path_ref.is_absolute() {
			Ok(path)
		} else {
			Ok(Cow::Owned(env::current_dir()?.join(path_ref)))
		}
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tempfile::TempDir;

	use super::*;

	#[test]
	fn test_resolve_finds_root_and_base() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		// Create project structure
		fs::write(root.join("jsonnetfile.json"), r#"{"version": 1}"#).unwrap();
		fs::create_dir_all(root.join("environments/test")).unwrap();
		fs::write(root.join("environments/test/main.jsonnet"), "{}").unwrap();

		let jpath = JPath::resolve(root.join("environments/test").to_str().unwrap()).unwrap();

		assert_eq!(jpath.root_directory, root);
		assert_eq!(jpath.base_directory, root.join("environments/test"));
		assert_eq!(
			jpath.entrypoint,
			root.join("environments/test/main.jsonnet")
		);
	}

	#[test]
	fn test_resolve_uses_tkrc_over_jsonnetfile() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		// Create both marker files - tkrc.yaml should take precedence
		fs::write(root.join("tkrc.yaml"), "").unwrap();
		fs::write(root.join("jsonnetfile.json"), r#"{"version": 1}"#).unwrap();
		fs::write(root.join("main.jsonnet"), "{}").unwrap();

		let jpath = JPath::resolve(root.to_str().unwrap()).unwrap();
		assert_eq!(jpath.root_directory, root);
	}

	#[test]
	fn test_resolve_uses_the_outermost_nested_project() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();
		let nested = root.join("environments/demo");

		fs::create_dir_all(&nested).unwrap();
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::write(root.join("main.jsonnet"), "{}").unwrap();
		fs::write(nested.join("jsonnetfile.json"), "{}").unwrap();
		fs::write(nested.join("main.jsonnet"), "{}").unwrap();

		let jpath = JPath::resolve(&nested).unwrap();
		assert_eq!(jpath.root_directory, root);
		assert_eq!(jpath.base_directory, root);
		assert_eq!(jpath.entrypoint, root.join("main.jsonnet"));
	}

	#[test]
	fn test_resolve_import_paths_order() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();

		let jpath = JPath::resolve(root.join("env").to_str().unwrap()).unwrap();

		// Import paths should be: [base, lib, base/vendor, root/vendor]
		assert_eq!(jpath.import_paths.len(), 4);
		assert_eq!(jpath.import_paths[0], root.join("env"));
		assert_eq!(jpath.import_paths[1], root.join("lib"));
		assert_eq!(jpath.import_paths[2], root.join("env/vendor"));
		assert_eq!(jpath.import_paths[3], root.join("vendor"));
	}

	#[test]
	fn test_resolve_no_root_fails() {
		let temp = TempDir::new().unwrap();
		// Don't create jsonnetfile.json or tkrc.yaml
		fs::write(temp.path().join("main.jsonnet"), "{}").unwrap();

		let result = JPath::resolve(temp.path().to_str().unwrap());
		assert!(result.is_err());
		assert!(
			result
				.unwrap_err()
				.to_string()
				.contains("could not find project root")
		);
	}

	#[test]
	fn test_resolve_custom_entrypoint() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::write(root.join("custom.jsonnet"), "{}").unwrap();

		let jpath = JPath::resolve(root.join("custom.jsonnet").to_str().unwrap()).unwrap();
		assert_eq!(jpath.entrypoint, root.join("custom.jsonnet"));
	}

	#[test]
	fn test_resolve_deeply_nested_env() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		// Create deeply nested structure like deployment_tools
		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("ksonnet/environments/cortex/ops-us-west")).unwrap();
		fs::write(
			root.join("ksonnet/environments/cortex/ops-us-west/main.jsonnet"),
			"{}",
		)
		.unwrap();

		let jpath = JPath::resolve(
			root.join("ksonnet/environments/cortex/ops-us-west")
				.to_str()
				.unwrap(),
		)
		.unwrap();

		assert_eq!(jpath.root_directory, root);
		assert_eq!(
			jpath.base_directory,
			root.join("ksonnet/environments/cortex/ops-us-west")
		);
	}

	#[test]
	fn test_resolve_with_vendor_directories() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("vendor")).unwrap();
		fs::create_dir_all(root.join("lib")).unwrap();
		fs::create_dir_all(root.join("env/vendor")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();

		let jpath = JPath::resolve(root.join("env").to_str().unwrap()).unwrap();

		// Verify all expected paths are in import_paths
		assert!(jpath.import_paths.contains(&root.join("vendor")));
		assert!(jpath.import_paths.contains(&root.join("lib")));
		assert!(jpath.import_paths.contains(&root.join("env/vendor")));
		assert!(jpath.import_paths.contains(&root.join("env")));
	}

	#[test]
	fn test_resolve_file_path_directly() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		fs::write(root.join("env/main.jsonnet"), "{}").unwrap();

		// Pass the file path directly instead of directory
		let jpath = JPath::resolve(root.join("env/main.jsonnet").to_str().unwrap()).unwrap();

		assert_eq!(jpath.base_directory, root.join("env"));
		assert_eq!(jpath.entrypoint, root.join("env/main.jsonnet"));
	}

	#[test]
	fn test_resolve_no_main_jsonnet_fails() {
		let temp = TempDir::new().unwrap();
		let root = temp.path();

		fs::write(root.join("jsonnetfile.json"), "{}").unwrap();
		fs::create_dir_all(root.join("env")).unwrap();
		// Don't create main.jsonnet

		let jpath = JPath::resolve(root.join("env").to_str().unwrap());
		assert!(jpath.is_err());
		assert!(
			jpath
				.unwrap_err()
				.to_string()
				.contains("could not find environment base")
		);
	}
}
