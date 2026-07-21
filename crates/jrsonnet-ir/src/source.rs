use std::{
	any::Any,
	fmt::{self, Debug, Display},
	hash::{Hash, Hasher},
	path::{Path, PathBuf},
	rc::Rc,
};

use jrsonnet_gcmodule::Acyclic;
use jrsonnet_interner::{IBytes, IStr};
use url::Url;

use crate::location::{CodeLocation, location_to_offset, offset_to_location};

macro_rules! any_ext_methods {
	($T:ident) => {
		/// Object-safe Hash implementation, usually provided by [`any_ext_impl`] macro.
		fn dyn_hash(&self, hasher: &mut dyn Hasher);
		/// Object-safe PartialEq/Eq implementation, usually provided by [`any_ext_impl`] macro.
		fn dyn_eq(&self, other: &dyn $T) -> bool;
	};
}
macro_rules! any_ext_impl {
	($T:ident) => {
		fn dyn_hash(&self, mut hasher: &mut dyn Hasher) {
			self.hash(&mut hasher)
		}
		fn dyn_eq(&self, other: &dyn $T) -> bool {
			let other: &dyn Any = &*other;
			let Some(other) = other.downcast_ref::<Self>() else {
				return false;
			};
			let this: &dyn Any = &*self;
			let this = this.downcast_ref::<Self>().expect("restricted by impl");
			this == other
		}
	};
}
macro_rules! any_ext {
	($T:ident) => {
		impl Hash for dyn $T {
			fn hash<H: Hasher>(&self, state: &mut H) {
				self.dyn_hash(state)
			}
		}
		impl PartialEq for dyn $T {
			fn eq(&self, other: &Self) -> bool {
				self.dyn_eq(other)
			}
		}
		impl Eq for dyn $T {}
	};
}
/// Represents trait methods used by [`SourcePath`] implementation.
pub trait SourcePathT: Acyclic + Debug + Display + Any {
	/// This method should be checked by resolver before panicking with bad SourcePath input
	/// if `true` - then resolver may threat this path as default, and default is usally a CWD.
	fn is_default(&self) -> bool;
	/// If this source path is backed by Os [`Path`] - it can be obtained from here.
	fn path(&self) -> Option<&Path>;
	any_ext_methods!(SourcePathT);
}
any_ext!(SourcePathT);

/// Represents location of a file.
///
/// Standard CLI only operates using
/// - [`SourceFile`] - for any file.
/// - [`SourceDirectory`] - for resolution from CWD.
/// - [`SourceVirtual`] - for stdlib/ext-str.
/// - [`SourceFifo`] - for /dev/fd/X (This path may appear with `jrsonnet <(command_that_produces_jsonnet)`).
///
/// From all of those, only [`SourceVirtual`] may be constructed manually, any other path kind should be only obtained
/// from assigned `ImportResolver`.
/// However, you should always check `is_default` method return, as it will return true for any paths, where default
/// search location is applicable.
///
/// Resolver may also return custom implementations of this trait, for example it may return http url in case of
/// remotely loaded files.
#[derive(Eq, Clone, Acyclic)]
pub struct SourcePath(Rc<dyn SourcePathT>);
impl SourcePath {
	/// Create [`SourcePath`] structure from a unboxed [`SourcePathT`]
	pub fn new(inner: impl SourcePathT) -> Self {
		Self(Rc::new(inner))
	}
	/// Try to downcast the inner boxed [`SourcePathT`] to the specific type.
	pub fn downcast_ref<T: SourcePathT>(&self) -> Option<&T> {
		let this: &dyn Any = &*self.0;
		this.downcast_ref()
	}
	/// This method should be checked by resolver before panicking with bad SourcePath input
	/// if `true` - then resolver may threat this path as default, and default is usally a CWD.
	pub fn is_default(&self) -> bool {
		self.0.is_default()
	}
	/// If this source path is backed by Os [`Path`] - it can be obtained from here.
	pub fn path(&self) -> Option<&Path> {
		self.0.path()
	}
}
impl Hash for SourcePath {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.0.hash(state);
	}
}
impl PartialEq for SourcePath {
	#[allow(clippy::op_ref)]
	fn eq(&self, other: &Self) -> bool {
		&*self.0 == &*other.0
	}
}
impl Display for SourcePath {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.0)
	}
}
impl Debug for SourcePath {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{:?}", self.0)
	}
}
impl Default for SourcePath {
	fn default() -> Self {
		Self(Rc::new(SourceDefault))
	}
}

/// Used as a search base, represents default search path, including JPATH.
/// Used for import resolution for `--ext-str` and other methods without definitive import source.
#[derive(Acyclic, Hash, PartialEq, Eq, Debug)]
pub struct SourceDefault;
impl Display for SourceDefault {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "<default>")
	}
}
impl SourcePathT for SourceDefault {
	fn is_default(&self) -> bool {
		true
	}
	fn path(&self) -> Option<&Path> {
		None
	}
	any_ext_impl!(SourcePathT);
}

/// Used as a search base, represents default search path, excluding JPATH.
/// Used by default by jrsonnet cli.
#[derive(Acyclic, Hash, PartialEq, Eq, Debug)]
pub struct SourceDefaultIgnoreJpath;
impl Display for SourceDefaultIgnoreJpath {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "<default (ignoring jpath)>")
	}
}
impl SourcePathT for SourceDefaultIgnoreJpath {
	fn is_default(&self) -> bool {
		true
	}
	fn path(&self) -> Option<&Path> {
		None
	}
	any_ext_impl!(SourcePathT);
}

/// Represents path to the file on the disk.
///
/// Directories shouldn't be put here, as resolution for files differs from resolution for directories:
///
/// When `file` is being resolved from `SourceFile(a/b/c)`, it should be resolved to `SourceFile(a/b/file)`,
/// however if it is being resolved from `SourceDirectory(a/b/c)`, then it should be resolved to `SourceDirectory(a/b/c/file)`
#[derive(Acyclic, Hash, PartialEq, Eq, Debug)]
pub struct SourceFile(PathBuf);
impl SourceFile {
	/// Wrap a path buf for [`SourceFile`]
	pub fn new(path: PathBuf) -> Self {
		Self(path)
	}
	/// Retrieve a wrapped [`Path`]
	pub fn path(&self) -> &Path {
		&self.0
	}
}
impl Display for SourceFile {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.0.display())
	}
}
impl SourcePathT for SourceFile {
	fn is_default(&self) -> bool {
		false
	}
	fn path(&self) -> Option<&Path> {
		Some(&self.0)
	}
	any_ext_impl!(SourcePathT);
}

/// Represents path to the file by URL, used for WASM builds, and UNUSABLE by the default import resolvers.
///
/// Note that it might contain file: urls, but they are not returned in .path() getter (TODO: should it be?..),
/// since we have no defined OsStr encoding for wasm.
#[derive(Acyclic, Hash, PartialEq, Eq, Debug)]
pub struct SourceUrl(Url);
impl SourceUrl {
	/// Wrap a url for [`SourceUrl`]
	pub fn new(url: Url) -> Self {
		Self(url)
	}
}
impl Display for SourceUrl {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.0)
	}
}
impl SourcePathT for SourceUrl {
	fn is_default(&self) -> bool {
		false
	}
	fn path(&self) -> Option<&Path> {
		// TODO: Parse file:?
		None
	}
	any_ext_impl!(SourcePathT);
}

/// Represents path to the directory on the disk.
///
/// See also [`SourceFile`].
#[derive(Acyclic, Hash, PartialEq, Eq, Debug)]
pub struct SourceDirectory(PathBuf);
impl SourceDirectory {
	/// Wrap a path buf pointing to directory for [`SourceDirectory`]
	pub fn new(path: PathBuf) -> Self {
		Self(path)
	}
	/// Retrieve a wrapped [`Path`]
	pub fn path(&self) -> &Path {
		&self.0
	}
}
impl Display for SourceDirectory {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}", self.0.display())
	}
}
impl SourcePathT for SourceDirectory {
	fn is_default(&self) -> bool {
		false
	}
	fn path(&self) -> Option<&Path> {
		Some(&self.0)
	}
	any_ext_impl!(SourcePathT);
}

/// Represents virtual file, whose are located in memory, and shouldn't be cached
///
/// It is used for --ext-code=.../--tla-code=.../standard library source code by default,
/// and user can construct arbitrary values by hand, without asking import resolver
#[derive(Acyclic, Hash, PartialEq, Eq, Clone)]
pub struct SourceVirtual(pub IStr);
impl Display for SourceVirtual {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "virtual:{}", self.0)
	}
}
impl fmt::Debug for SourceVirtual {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "virtual:{}", self.0)
	}
}
impl SourcePathT for SourceVirtual {
	fn is_default(&self) -> bool {
		true
	}
	fn path(&self) -> Option<&Path> {
		None
	}
	any_ext_impl!(SourcePathT);
}

/// Represents resolved FIFO file, those files may only be read once, and this type is only used for
/// unix, where user might want to do `jrsonnet <(command_that_produces_jsonnet_source)`
/// In most cases, user most probably want to use `jrsonnet -` instead of `jrsonnet /dev/stdin`
/// for better cross-platform support.
// PartialEq is limited to ptr equality
#[allow(clippy::derived_hash_with_manual_eq)]
#[derive(Acyclic, Debug, Hash)]
pub struct SourceFifo(pub String, pub IBytes);
impl PartialEq for SourceFifo {
	fn eq(&self, other: &Self) -> bool {
		std::ptr::eq(self, other)
	}
}
impl fmt::Display for SourceFifo {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "fifo({:?})", self.0)
	}
}
impl SourcePathT for SourceFifo {
	fn is_default(&self) -> bool {
		// In case of FD input, user won't expect relative paths to be resolved from /dev/fd/
		true
	}

	fn path(&self) -> Option<&Path> {
		None
	}

	any_ext_impl!(SourcePathT);
}

/// Either real file, or virtual
/// Hash of FileName always have same value as raw Path, to make it possible to use with raw_entry_mut
#[derive(Clone, PartialEq, Eq, Acyclic)]
pub struct Source(pub Rc<(SourcePath, IStr)>);

impl Source {
	/// Wrap a source path and source code.
	pub fn new(path: SourcePath, code: IStr) -> Self {
		Self(Rc::new((path, code)))
	}

	/// Create a virtual source for the given name and source code.
	pub fn new_virtual(name: IStr, code: IStr) -> Self {
		Self::new(SourcePath::new(SourceVirtual(name)), code)
	}

	/// Retrieve the stored source code.
	pub fn code(&self) -> &str {
		&self.0.1
	}

	/// Retrieve the stored file path.
	pub fn source_path(&self) -> &SourcePath {
		&self.0.0
	}

	/// Convert span locations into line number+column+extra information for diagnostics/lsp.
	///
	/// See [`CodeLocation`].
	pub fn map_source_locations<const S: usize>(&self, locs: &[u32; S]) -> [CodeLocation; S] {
		offset_to_location(&self.0.1, locs)
	}
	/// Unconvert the line number+column from the representaion used by [`CodeLocation`] to the span offset.
	pub fn map_from_source_location(&self, line: usize, column: usize) -> Option<usize> {
		location_to_offset(&self.0.1, line, column)
	}
}
impl fmt::Debug for Source {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{:?}", self.0.0)
	}
}
