use anyhow::{Context, Result};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_ENTRYPOINT: &str = "main.jsonnet";

#[derive(Clone)]
struct CachedJsonnetFile {
	base: String,
	imports: Vec<String>,
	is_main_file: bool,
}

pub fn find_importers(root: &str, files: Vec<String>) -> Result<Vec<String>> {
	let root = fs::canonicalize(root).context("resolving root")?;
	let root_str = root.to_string_lossy().to_string();

	let mut importers_set = HashSet::new();

	// Handle files prefixed with `deleted:`. They need to be made absolute and we shouldn't try to find symlinks for them
	let mut files_to_check = Vec::new();
	let mut existing_files = Vec::new();

	for file in files {
		if file.starts_with("deleted:") {
			let deleted_file = file.trim_start_matches("deleted:");
			let deleted_path = Path::new(deleted_file);

			if !deleted_path.is_absolute() {
				// Try with both the absolute path and the path relative to the root
				if let Ok(abs_path) = fs::canonicalize(deleted_file) {
					files_to_check.push(abs_path.to_string_lossy().to_string());
				}
				let root_relative = root.join(deleted_file);
				files_to_check.push(root_relative.to_string_lossy().to_string());
			} else {
				files_to_check.push(deleted_file.to_string());
			}
			continue;
		}

		if !Path::new(&file).exists() {
			anyhow::bail!("file {:?} does not exist", file);
		}

		existing_files.push(file);
	}

	// Expand symlinks for existing files
	let expanded_files = expand_symlinks_in_files(&root_str, existing_files)?;
	files_to_check.extend(expanded_files);

	// Create a shared cache
	let mut jsonnet_files_cache: HashMap<String, HashMap<String, CachedJsonnetFile>> =
		HashMap::new();
	let mut importers_cache: HashMap<String, Vec<String>> = HashMap::new();

	// Loop through all given files and add their importers to the list
	for file in &files_to_check {
		importers_set.insert(file.clone());
		let new_importers = find_importers_recursive(
			&root_str,
			file,
			&mut HashSet::new(),
			&mut jsonnet_files_cache,
			&mut importers_cache,
		)?;

		for importer in new_importers {
			let eval_importer = eval_symlinks(&importer)?;
			importers_set.insert(eval_importer);
		}
	}

	// Filter to only main files
	let mut main_files: Vec<String> = importers_set
		.into_iter()
		.filter(|path| {
			Path::new(path)
				.file_name()
				.and_then(|n| n.to_str())
				.map(|n| n == DEFAULT_ENTRYPOINT)
				.unwrap_or(false)
		})
		.collect();

	main_files.sort();
	Ok(main_files)
}

fn expand_symlinks_in_files(root: &str, files: Vec<String>) -> Result<Vec<String>> {
	let mut files_map = HashSet::new();

	for file in files {
		let abs_file = fs::canonicalize(&file).context("making file absolute")?;
		let abs_file_str = abs_file.to_string_lossy().to_string();
		files_map.insert(abs_file_str.clone());

		// Add the file after evaluating symlinks
		let symlink_eval = eval_symlinks(&abs_file_str)?;
		if symlink_eval != abs_file_str {
			files_map.insert(symlink_eval);
		}

		// Find all symlinks that point to this file
		let symlinks = find_symlinks(root, &abs_file_str)?;
		for symlink in symlinks {
			files_map.insert(symlink);
		}
	}

	let mut result: Vec<String> = files_map.into_iter().collect();
	result.sort();
	Ok(result)
}

fn eval_symlinks(path: &str) -> Result<String> {
	let path_buf = Path::new(path);
	if !path_buf.exists() {
		return Ok(path.to_string());
	}

	match fs::canonicalize(path) {
		Ok(p) => Ok(p.to_string_lossy().to_string()),
		Err(_) => Ok(path.to_string()),
	}
}

fn find_symlinks(root: &str, file: &str) -> Result<Vec<String>> {
	let mut symlinks = Vec::new();
	let root_path = Path::new(root);

	// Walk the directory tree looking for symlinks
	for entry in walkdir::WalkDir::new(root_path).follow_links(false) {
		let entry = entry?;
		let path = entry.path();

		if path.is_symlink() {
			if let Ok(link_target) = fs::read_link(path) {
				// Resolve the link target
				let resolved = if link_target.is_absolute() {
					link_target
				} else {
					path.parent().unwrap_or(Path::new("/")).join(link_target)
				};

				if let Ok(canonical_target) = fs::canonicalize(&resolved) {
					let canonical_target_str = canonical_target.to_string_lossy().to_string();
					if file.contains(&canonical_target_str) {
						let symlink_path = path.to_string_lossy().to_string();
						let result = file.replace(&canonical_target_str, &symlink_path);
						symlinks.push(result);
					}
				}
			}
		}
	}

	Ok(symlinks)
}

fn find_importers_recursive(
	root: &str,
	search_for_file: &str,
	chain: &mut HashSet<String>,
	jsonnet_files_cache: &mut HashMap<String, HashMap<String, CachedJsonnetFile>>,
	importers_cache: &mut HashMap<String, Vec<String>>,
) -> Result<Vec<String>> {
	// If we've already looked through this file in the current execution, don't do it again
	if chain.contains(search_for_file) {
		return Ok(Vec::new());
	}
	chain.insert(search_for_file.to_string());

	// Check cache
	let cache_key = format!("{}:{}", root, search_for_file);
	if let Some(cached) = importers_cache.get(&cache_key) {
		return Ok(cached.clone());
	}

	let jsonnet_files = create_jsonnet_file_cache(root, jsonnet_files_cache)?;

	let mut importers = Vec::new();
	let mut intermediate_importers = Vec::new();

	// Optimization: if the file is not in vendor/ or lib/, assume it's in an environment
	let root_vendor = Path::new(root).join("vendor");
	let root_lib = Path::new(root).join("lib");

	let is_file_lib_or_vendored = |file: &str| -> bool {
		let file_path = Path::new(file);
		file_path.starts_with(&root_vendor) || file_path.starts_with(&root_lib)
	};

	let searched_file_is_lib_or_vendored = is_file_lib_or_vendored(search_for_file);

	if !searched_file_is_lib_or_vendored {
		let searched_dir = Path::new(search_for_file)
			.parent()
			.unwrap_or(Path::new("/"));

		if let Some(entrypoint) = find_entrypoint(searched_dir) {
			// Found the main file for the searched file, add it as an importer
			importers.push(entrypoint);
		} else if searched_dir.exists() {
			// No main file found, add all main files in child dirs as importers
			let files = find_jsonnet_files(searched_dir)?;
			for file in files {
				if Path::new(&file)
					.file_name()
					.and_then(|n| n.to_str())
					.map(|n| n == DEFAULT_ENTRYPOINT)
					.unwrap_or(false)
				{
					importers.push(file);
				}
			}
		}
	}

	// Check all jsonnet files for imports
	for (jsonnet_file_path, jsonnet_file_content) in jsonnet_files.iter() {
		if jsonnet_file_content.imports.is_empty() {
			continue;
		}

		let mut is_importer = false;

		for import_path in &jsonnet_file_content.imports {
			// If the filename is not the same as the file we are looking for, skip it
			let import_basename = Path::new(import_path)
				.file_name()
				.and_then(|n| n.to_str())
				.unwrap_or("");
			let search_basename = Path::new(search_for_file)
				.file_name()
				.and_then(|n| n.to_str())
				.unwrap_or("");

			if import_basename != search_basename {
				continue;
			}

			// Clean the import path
			let import_path_clean = Path::new(import_path)
				.components()
				.collect::<PathBuf>()
				.to_string_lossy()
				.to_string();

			// Match on relative imports with ..
			if import_path.starts_with("..") {
				let jsonnet_dir = Path::new(jsonnet_file_path)
					.parent()
					.unwrap_or(Path::new("/"));

				// Shallow import (one less level of ..)
				let shallow_import = import_path_clean.replacen("../", "", 1);
				let shallow_import_path = jsonnet_dir.join(&shallow_import);
				let shallow_import_clean = shallow_import_path
					.components()
					.collect::<PathBuf>()
					.to_string_lossy()
					.to_string();

				// Full import
				let import_full_path = jsonnet_dir.join(&import_path_clean);
				let import_full_clean = import_full_path
					.components()
					.collect::<PathBuf>()
					.to_string_lossy()
					.to_string();

				is_importer = path_matches(search_for_file, &import_full_clean)
					|| path_matches(search_for_file, &shallow_import_clean);
			}

			// Match on imports to lib/ or vendor/
			if !is_importer {
				let vendor_path = root_vendor.join(&import_path_clean);
				let lib_path = root_lib.join(&import_path_clean);
				is_importer = path_matches(search_for_file, &vendor_path.to_string_lossy())
					|| path_matches(search_for_file, &lib_path.to_string_lossy());
			}

			// Match on imports to the base dir where the file is located
			if !is_importer {
				let base = if jsonnet_file_content.base.is_empty() {
					find_base(jsonnet_file_path, root)?
				} else {
					jsonnet_file_content.base.clone()
				};

				// Check if the search file is in the base directory and ends with the import path
				// But also ensure that the path segment before the import path in search_for_file
				// matches the path segment in the base (to avoid false positives)
				if search_for_file.starts_with(&base) && search_for_file.ends_with(import_path) {
					// Extract the part between base and the file
					let relative_to_base = search_for_file.strip_prefix(&base).unwrap_or("");
					let relative_to_base = relative_to_base.trim_start_matches('/');

					// The relative path should match the import path exactly
					is_importer = relative_to_base == import_path;
				}
			}

			// Also check if the import is relative to the directory of the importing file
			// This handles cases like 'text-file.txt' imported from 'vendor/vendored/main.libsonnet'
			if !is_importer {
				let importer_dir = Path::new(jsonnet_file_path)
					.parent()
					.unwrap_or(Path::new("/"));
				let import_full_path = importer_dir.join(import_path);
				let import_full_str = import_full_path.to_string_lossy().to_string();
				is_importer = path_matches(search_for_file, &import_full_str);
			}

			if is_importer {
				if jsonnet_file_content.is_main_file {
					importers.push(jsonnet_file_path.clone());
				}
				intermediate_importers.push(jsonnet_file_path.clone());
				break;
			}
		}
	}

	// Process intermediate importers recursively
	if !intermediate_importers.is_empty() {
		for intermediate_importer in &intermediate_importers {
			importers.push(intermediate_importer.clone());
			let new_importers = find_importers_recursive(
				root,
				intermediate_importer,
				chain,
				jsonnet_files_cache,
				importers_cache,
			)?;
			importers.extend(new_importers);
		}
	}

	// Filter out vendored files that are overridden
	let filtered_importers = if search_for_file.starts_with(root_vendor.to_str().unwrap_or("")) {
		let mut filtered = Vec::new();
		for importer in &importers {
			if let Ok(rel_path) = Path::new(search_for_file).strip_prefix(&root_vendor) {
				let vendored_in_env = Path::new(importer)
					.parent()
					.unwrap_or(Path::new("/"))
					.join("vendor")
					.join(rel_path);
				let vendored_in_env_str = vendored_in_env.to_string_lossy().to_string();

				if !jsonnet_files.contains_key(&vendored_in_env_str) {
					filtered.push(importer.clone());
				}
			}
		}
		filtered
	} else {
		importers
	};

	importers_cache.insert(cache_key, filtered_importers.clone());
	Ok(filtered_importers)
}

fn create_jsonnet_file_cache(
	root: &str,
	cache: &mut HashMap<String, HashMap<String, CachedJsonnetFile>>,
) -> Result<HashMap<String, CachedJsonnetFile>> {
	if let Some(cached) = cache.get(root) {
		return Ok(cached.clone());
	}

	let mut files_map = HashMap::new();
	let files = find_jsonnet_files(Path::new(root))?;
	let imports_regexp = Regex::new(r#"import(str)?\s+['"]([^'"%()]+)['"]"#)?;

	for file in files {
		let content = fs::read_to_string(&file).context(format!("reading file {}", file))?;
		let is_main_file = file.ends_with(DEFAULT_ENTRYPOINT);

		let mut imports = Vec::new();
		for cap in imports_regexp.captures_iter(&content) {
			if let Some(import_path) = cap.get(2) {
				imports.push(import_path.as_str().to_string());
			}
		}

		files_map.insert(
			file,
			CachedJsonnetFile {
				base: String::new(),
				imports,
				is_main_file,
			},
		);
	}

	cache.insert(root.to_string(), files_map.clone());
	Ok(files_map)
}

fn find_jsonnet_files(dir: &Path) -> Result<Vec<String>> {
	let mut files = Vec::new();

	for entry in walkdir::WalkDir::new(dir) {
		let entry = entry?;
		let path = entry.path();

		if !path.is_file() {
			continue;
		}

		if let Some(ext) = path.extension() {
			let ext_str = ext.to_string_lossy();
			if ext_str == "jsonnet" || ext_str == "libsonnet" {
				files.push(path.to_string_lossy().to_string());
			}
		}
	}

	Ok(files)
}

fn find_entrypoint(dir: &Path) -> Option<String> {
	let mut current_dir = dir;

	// Walk up the directory tree
	loop {
		if !current_dir.exists() {
			if let Some(parent) = current_dir.parent() {
				current_dir = parent;
				continue;
			} else {
				break;
			}
		}

		let entrypoint = current_dir.join(DEFAULT_ENTRYPOINT);
		if entrypoint.exists() {
			return Some(entrypoint.to_string_lossy().to_string());
		}

		// Try to go to parent
		if let Some(parent) = current_dir.parent() {
			current_dir = parent;
		} else {
			break;
		}
	}

	None
}

fn find_base(path: &str, root: &str) -> Result<String> {
	let path_buf = Path::new(path);
	let root_buf = Path::new(root);

	// Start from the file's directory and walk up
	let mut current = if path_buf.is_file() {
		path_buf.parent().unwrap_or(Path::new("/"))
	} else {
		path_buf
	};

	while current.starts_with(root_buf) {
		let main_file = current.join(DEFAULT_ENTRYPOINT);
		if main_file.exists() {
			return Ok(current.to_string_lossy().to_string());
		}

		if let Some(parent) = current.parent() {
			current = parent;
		} else {
			break;
		}
	}

	// If no main.jsonnet found, return the root
	Ok(root.to_string())
}

fn path_matches(path1: &str, path2: &str) -> bool {
	if path1 == path2 {
		return true;
	}

	let eval1 = eval_symlinks(path1).unwrap_or_else(|_| path1.to_string());
	let eval2 = eval_symlinks(path2).unwrap_or_else(|_| path2.to_string());

	eval1 == eval2
}
