//! Shared scanning primitives for import discovery.
//!
//! Both [`crate::imports`] and [`crate::importers`] discover dependencies by
//! scanning jsonnet sources for `import`/`importstr` statements and for
//! helmTemplate/kustomizeBuild chart directory references. [`Source`] and
//! [`ChartPath`] keep the two in sync, and [`PathExt`] carries the path
//! predicates they share.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::jpath::JPath;

/// Matches `import` and `importstr` statements. Capture 1 is the optional
/// `str` suffix, capture 2 the imported path literal.
static IMPORT_RE: LazyLock<Regex> =
	LazyLock::new(|| Regex::new(r#"import(str)?\s+['"]([^'"%()]+)['"]"#).expect("valid regex"));

/// Matches chart/kustomize directory references; capture 1 is the chart path.
static CHART_DIR_RES: LazyLock<[Regex; 4]> = LazyLock::new(|| {
	[
		// Direct helmTemplate - 2nd positional arg is the chart path
		r#"std\.native\(\s*['"]helmTemplate['"]\s*\)\s*\([^,]+,\s*['"]([^'"]+)['"]"#,
		// Wrapper helmTemplate - .template(name, 'chart-path'), where the
		// first arg can be a string literal or a variable
		r#"\.template\(\s*[^,]+,\s*['"]([^'"]+)['"]"#,
		// Direct kustomizeBuild - 1st positional arg is the path
		r#"std\.native\(\s*['"]kustomizeBuild['"]\s*\)\s*\(\s*['"]([^'"]+)['"]"#,
		// Wrapper kustomizeBuild - .build('path')
		r#"\.build\(\s*['"]([^'"]+)['"]"#,
	]
	.map(|pattern| Regex::new(pattern).expect("valid regex"))
});

/// A jsonnet source text, scannable for the references that make other files
/// part of an environment.
#[derive(Clone, Copy)]
pub(crate) struct Source<'a>(&'a str);

/// An `import`/`importstr` statement found in a [`Source`].
pub(crate) struct Import<'a> {
	/// The imported path literal, verbatim from the source.
	pub(crate) path: &'a str,
	/// Whether this is `importstr` (plain text, never recursed into).
	pub(crate) importstr: bool,
}

impl<'a> Source<'a> {
	pub(crate) fn new(content: &'a str) -> Self {
		Source(content)
	}

	/// The `import`/`importstr` statements in the source.
	pub(crate) fn imports(self) -> impl Iterator<Item = Import<'a>> {
		IMPORT_RE.captures_iter(self.0).filter_map(|capture| {
			Some(Import {
				path: capture.get(2)?.as_str(),
				importstr: capture.get(1).is_some(),
			})
		})
	}

	/// The chart/kustomize directory references in the source.
	pub(crate) fn chart_paths(self) -> impl Iterator<Item = ChartPath<'a>> {
		CHART_DIR_RES
			.iter()
			.flat_map(move |re| re.captures_iter(self.0))
			.filter_map(|capture| Some(ChartPath(capture.get(1)?.as_str())))
	}
}

/// A chart/kustomize directory reference found in a [`Source`]. May be a full
/// static path or a prefix used in string concatenation or interpolation
/// (e.g. `'./charts/' + version`, `'./charts/%s' % v`).
pub(crate) struct ChartPath<'a>(&'a str);

impl ChartPath<'_> {
	/// Resolve to canonical directories, relative to the referencing file's
	/// directory.
	///
	/// Static paths like `./charts/my-chart` resolve directly. Dynamic paths
	/// like `./charts/%s` or `./charts/` match every subdirectory whose name
	/// starts with the static prefix portion.
	pub(crate) fn resolve_dirs(&self, file_dir: &Path) -> Vec<PathBuf> {
		if let Ok(dir) = fs::canonicalize(file_dir.join(self.0))
			&& dir.is_dir()
		{
			return vec![dir];
		}

		// The path didn't resolve directly - it likely contains a format
		// specifier (%s) or is a prefix used with string concatenation. Strip
		// the dynamic suffix to get the parent directory, then match
		// subdirectories against the remaining static prefix.
		let clean = self.0.trim_end_matches('/').replace("%s", "");
		let clean = Path::new(&clean);
		let Some(parent) = clean.parent() else {
			return Vec::new();
		};
		let Ok(parent) = fs::canonicalize(file_dir.join(parent)) else {
			return Vec::new();
		};
		let Ok(entries) = fs::read_dir(&parent) else {
			return Vec::new();
		};

		let prefix = clean
			.file_name()
			.and_then(|name| name.to_str())
			.unwrap_or("");
		entries
			.flatten()
			.map(|entry| entry.path())
			.filter(|path| path.is_dir())
			.filter(|path| {
				prefix.is_empty()
					|| path
						.file_name()
						.and_then(|name| name.to_str())
						.is_some_and(|name| name.starts_with(prefix))
			})
			.collect()
	}
}

/// Path predicates and helpers shared by import discovery.
pub(crate) trait PathExt {
	/// Whether the file is jsonnet source that can contain further imports.
	fn is_jsonnet_file(&self) -> bool;

	/// Whether a file inside a chart/kustomize directory could affect
	/// template output. Excludes documentation (.md) and license files.
	fn is_chart_relevant_file(&self) -> bool;

	/// Whether the path is an environment entrypoint (main.jsonnet).
	fn is_entrypoint(&self) -> bool;

	/// The parent directory, or the filesystem root for pathless paths.
	fn parent_or_root(&self) -> &Path;

	/// The canonical path, or the path unchanged when canonicalization fails.
	fn canonicalize_or_self(&self) -> PathBuf;

	/// Join and lexically normalize (without resolving `..` or symlinks).
	fn clean_join(&self, path: &Path) -> PathBuf;
}

impl PathExt for Path {
	fn is_jsonnet_file(&self) -> bool {
		self.extension()
			.is_some_and(|ext| ext == "jsonnet" || ext == "libsonnet")
	}

	fn is_chart_relevant_file(&self) -> bool {
		self.extension().is_none_or(|ext| ext != "md")
			&& self.file_name().is_none_or(|name| name != "LICENSE")
	}

	fn is_entrypoint(&self) -> bool {
		self.file_name()
			.is_some_and(|name| name == JPath::DEFAULT_ENTRYPOINT)
	}

	fn parent_or_root(&self) -> &Path {
		self.parent().unwrap_or_else(|| Path::new("/"))
	}

	fn canonicalize_or_self(&self) -> PathBuf {
		fs::canonicalize(self).unwrap_or_else(|_| self.to_owned())
	}

	fn clean_join(&self, path: &Path) -> PathBuf {
		self.join(path).components().collect()
	}
}
