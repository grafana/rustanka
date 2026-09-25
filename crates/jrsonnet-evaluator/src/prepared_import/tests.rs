use std::{any::Any, cell::Cell};

use super::*;
use crate::{
	AsPathLike, ContextInitializer, ImportResolver, InitialContextBuilder, Result, Source,
	SourceVirtual, State, Thunk, Val,
};

#[test]
fn prepared_cache_is_shared_by_threads_but_not_processes() {
	use std::process::Command;

	let name = std::env::var("RTK_PREPARED_IMPORT_CHILD")
		.unwrap_or_else(|_| format!("test-{}", std::process::id()));
	let path = SourcePath::new(SourceVirtual(name.clone().into()));
	let code: IStr = "{ answer: 42 }".into();
	let source = Source::new(path.clone(), code.clone());
	if std::env::var_os("RTK_PREPARED_IMPORT_CHILD").is_some() {
		assert!(shared_threads::get(&path, &code, &[], source).is_none());
		return;
	}
	let parsed = crate::parse_jsonnet(&code, source.clone()).unwrap();
	let report = crate::analyze::analyze_root(&parsed, Vec::new());
	assert!(!report.errored);
	shared_threads::insert(&path, &code, &[], source, &report.lir);
	std::thread::spawn(move || {
		let path = SourcePath::new(SourceVirtual(name.into()));
		let code: IStr = "{ answer: 42 }".into();
		let source = Source::new(path.clone(), code.clone());
		assert!(shared_threads::get(&path, &code, &[], source.clone()).is_some());
		assert!(
			shared_threads::get(&path, &"{ answer: 43 }".into(), &[], source.clone()).is_none()
		);
		assert!(
			shared_threads::get(&path, &code, &[("other".into(), LocalId(0))], source).is_none()
		);
	})
	.join()
	.unwrap();
	let result = Command::new(std::env::current_exe().unwrap())
		.args([
			"--exact",
			"prepared_import::tests::prepared_cache_is_shared_by_threads_but_not_processes",
		])
		.env(
			"RTK_PREPARED_IMPORT_CHILD",
			format!("test-{}", std::process::id()),
		)
		.output()
		.unwrap();
	assert!(
		result.status.success(),
		"{}",
		String::from_utf8_lossy(&result.stdout)
	);
}

#[derive(Clone, Default, Acyclic)]
struct Files {
	contents: Rc<RefCell<FxHashMap<String, String>>>,
	loads: Rc<Cell<usize>>,
	search: String,
}

impl Files {
	fn write(&self, name: &str, code: &str) {
		self.contents.borrow_mut().insert(name.into(), code.into());
	}

	fn state(&self, cache: &PreparedImportCache, bindings: &[(&str, u32)]) -> State {
		let mut builder = State::builder();
		builder.import_resolver(self.clone());
		builder.prepared_import_cache(cache.clone());
		builder.context_initializer(Bindings(
			bindings
				.iter()
				.map(|(name, value)| ((*name).into(), *value))
				.collect(),
		));
		builder.build()
	}

	fn eval(&self, cache: &PreparedImportCache, bindings: &[(&str, u32)]) -> Result<Val> {
		let state = self.state(cache, bindings);
		let _guard = state.enter();
		state.import("library")
	}
}

impl ImportResolver for Files {
	fn resolve_from(&self, from: &SourcePath, path: &dyn AsPathLike) -> Result<SourcePath> {
		let path = path.as_path().as_ref().to_string_lossy().into_owned();
		let parent = from
			.downcast_ref::<SourceVirtual>()
			.and_then(|source| source.0.rsplit_once('/').map(|(parent, _)| parent));
		let relative = parent.map_or_else(|| path.clone(), |parent| format!("{parent}/{path}"));
		let resolved = if self.contents.borrow().contains_key(&relative) {
			relative
		} else {
			format!("{}{path}", self.search)
		};
		Ok(SourcePath::new(SourceVirtual(resolved.into())))
	}

	fn load_file_contents(&self, path: &SourcePath) -> Result<Vec<u8>> {
		self.loads.set(self.loads.get() + 1);
		let name = &path.downcast_ref::<SourceVirtual>().unwrap().0;
		self.contents
			.borrow()
			.get(name.as_str())
			.map(|code| code.as_bytes().to_vec())
			.ok_or_else(|| crate::ImportIo(format!("missing {name}")).into())
	}
}

#[derive(Acyclic)]
struct Bindings(Vec<(String, u32)>);
impl ContextInitializer for Bindings {
	fn populate(&self, _source: Source, builder: &mut InitialContextBuilder) {
		for (name, value) in &self.0 {
			builder.bind(name.as_str(), Thunk::evaluated(Val::Num((*value).into())));
		}
	}
	fn as_any(&self) -> &dyn Any {
		self
	}
}

impl PreparedImportCache {
	fn cached_library(&self) -> Rc<LExpr> {
		self.0.borrow().entries[&SourcePath::new(SourceVirtual("library".into()))]
			.lir
			.clone()
	}

	fn limited(max_entries: usize, max_source_bytes: usize) -> Self {
		Self(Rc::new(RefCell::new(Cache {
			max_entries,
			max_source_bytes,
			..Cache::default()
		})))
	}
}

#[test]
fn reuses_lowered_code_but_reloads_files_and_binds_fresh_values() {
	let files = Files::default();
	let cache = PreparedImportCache::default();
	files.write("library", "x + 1");
	assert_eq!(files.eval(&cache, &[("x", 1)]).unwrap().as_num(), Some(2.0));
	let first = cache.cached_library();
	assert_eq!(files.eval(&cache, &[("x", 8)]).unwrap().as_num(), Some(9.0));
	assert!(Rc::ptr_eq(&first, &cache.cached_library()));
	assert_eq!(files.loads.get(), 2);
	files.contents.borrow_mut().remove("library");
	assert!(files.eval(&cache, &[("x", 8)]).is_err());
}

#[test]
fn changed_source_replaces_code_without_invalidating_existing_values() {
	let files = Files::default();
	let cache = PreparedImportCache::default();
	files.write("library", "{ value: x + 1 }");
	let old = files.eval(&cache, &[("x", 1)]).unwrap();
	let first = cache.cached_library();
	files.write("library", "{ value: x + 2 }");
	let new = files.eval(&cache, &[("x", 8)]).unwrap();
	let Val::Obj(old) = old else {
		panic!("expected object")
	};
	let Val::Obj(new) = new else {
		panic!("expected object")
	};
	assert_eq!(
		old.get("value".into()).unwrap().unwrap().as_num(),
		Some(2.0)
	);
	assert_eq!(
		new.get("value".into()).unwrap().unwrap().as_num(),
		Some(10.0)
	);
	assert!(!Rc::ptr_eq(&first, &cache.cached_library()));
	assert_eq!(cache.0.borrow().entries.len(), 1);
}

#[test]
fn root_binding_names_and_order_are_part_of_the_key() {
	let files = Files::default();
	let cache = PreparedImportCache::default();
	files.write("library", "x * 10 + y");
	assert_eq!(
		files.eval(&cache, &[("x", 1), ("y", 2)]).unwrap().as_num(),
		Some(12.0)
	);
	let first = cache.cached_library();
	assert_eq!(
		files.eval(&cache, &[("y", 3), ("x", 4)]).unwrap().as_num(),
		Some(43.0)
	);
	assert!(!Rc::ptr_eq(&first, &cache.cached_library()));
	assert!(files.eval(&cache, &[("x", 1), ("z", 2)]).is_err());
}

#[test]
fn invalid_sources_and_runtime_failures_never_supply_cached_values() {
	let files = Files::default();
	let cache = PreparedImportCache::default();
	files.write("library", "if x == 0 then error 'bad' else x");
	assert!(files.eval(&cache, &[("x", 0)]).is_err());
	let first = cache.cached_library();
	assert_eq!(files.eval(&cache, &[("x", 5)]).unwrap().as_num(), Some(5.0));
	assert!(Rc::ptr_eq(&first, &cache.cached_library()));
	for invalid in ["{", "undefined_local"] {
		files.write("library", invalid);
		assert!(files.eval(&cache, &[("x", 5)]).is_err());
		assert!(Rc::ptr_eq(&first, &cache.cached_library()));
	}
	files.write("library", "6");
	assert_eq!(files.eval(&cache, &[]).unwrap().as_num(), Some(6.0));
}

#[test]
fn cached_imports_keep_laziness_self_super_and_assertions() {
	let files = Files::default();
	let cache = PreparedImportCache::default();
	files.write("library", "local base = { n: x, value: self.n, unused: error 'lazy', assert self.n < 10 }; (base + { n: super.n + 2 }).value");
	assert_eq!(files.eval(&cache, &[("x", 1)]).unwrap().as_num(), Some(3.0));
	let first = cache.cached_library();
	assert_eq!(files.eval(&cache, &[("x", 4)]).unwrap().as_num(), Some(6.0));
	assert!(Rc::ptr_eq(&first, &cache.cached_library()));
}
#[test]
fn nested_imports_use_the_current_resolver_and_source_path() {
	let mut files = Files::default();
	let cache = PreparedImportCache::default();
	files.write("library", "import 'dependency'");
	files.write("a/dependency", "1");
	files.write("b/dependency", "2");
	files.search = "a/".into();
	assert_eq!(files.eval(&cache, &[]).unwrap().as_num(), Some(1.0));
	let first = cache.cached_library();
	files.search = "b/".into();
	assert_eq!(files.eval(&cache, &[]).unwrap().as_num(), Some(2.0));
	assert!(Rc::ptr_eq(&first, &cache.cached_library()));
	files.write("a/library", "import 'dependency'");
	files.write("b/library", "import 'dependency'");
	for (path, expected) in [("a/library", 1.0), ("b/library", 2.0)] {
		let state = files.state(&cache, &[]);
		let _guard = state.enter();
		assert_eq!(state.import(path).unwrap().as_num(), Some(expected));
	}
}

#[test]
fn fifo_limits_and_replacement_bound_retained_source() {
	let files = Files::default();
	let cache = PreparedImportCache::limited(2, 4);
	for (name, code) in [("one", "1"), ("two", "2"), ("three", "3")] {
		files.write(name, code);
		files.state(&cache, &[]).import(name).unwrap();
	}
	let inner = cache.0.borrow();
	assert_eq!(inner.entries.len(), 2);
	assert_eq!(inner.source_bytes, 2);
	assert!(
		!inner
			.entries
			.contains_key(&SourcePath::new(SourceVirtual("one".into())))
	);
	drop(inner);
	files.write("two", "2222");
	files.state(&cache, &[]).import("two").unwrap();
	assert_eq!(cache.0.borrow().source_bytes, 1);
	files.write("three", "33333");
	assert_eq!(
		files.state(&cache, &[]).import("three").unwrap().as_num(),
		Some(33333.0)
	);
	assert_eq!(cache.0.borrow().source_bytes, 1);
}

#[test]
fn bypassed_and_evicted_files_reuse_prepared_code_inside_the_state() {
	for cache in [
		PreparedImportCache::limited(0, 0),
		PreparedImportCache::limited(1, 100),
	] {
		let files = Files::default();
		files.write("library", "{ value: x }");
		files.write("other", "1");
		let state = files.state(&cache, &[("x", 7)]);
		let path = SourcePath::new(SourceVirtual("library".into()));
		drop(state.import("library").unwrap());
		let first = state.file_cache()[&path]
			.prepared
			.as_ref()
			.unwrap()
			.lir
			.clone();
		state.import("other").unwrap();
		drop(state.import("library").unwrap());
		let second = state.file_cache()[&path]
			.prepared
			.as_ref()
			.unwrap()
			.lir
			.clone();
		assert!(Rc::ptr_eq(&first, &second));
		assert_eq!(files.loads.get(), 2);
	}
}
