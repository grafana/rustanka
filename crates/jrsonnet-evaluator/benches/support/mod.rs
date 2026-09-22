use std::{any::Any, fmt::Write, hint::black_box, rc::Rc};

use jrsonnet_evaluator::{
	ContextInitializer, ImportResolver, InitialContextBuilder, PreparedImportCache, Source,
	SourcePath, SourceVirtual, State, Thunk, Val, manifest::JsonFormat,
};
use jrsonnet_gcmodule::Acyclic;

#[derive(Clone, Acyclic)]
pub struct Library {
	code: Rc<Vec<u8>>,
	path: SourcePath,
}

impl Library {
	pub fn new(fields: usize) -> Self {
		let mut code = String::from("local config = { offset: environment }; {\n");
		for field in 0..fields {
			writeln!(
				code,
				"field_{field}: {{ name: 'resource-{field}', value: config.offset + {field}, enabled: true }},"
			)
			.unwrap();
		}
		code.push('}');
		Self {
			code: Rc::new(code.into_bytes()),
			path: SourcePath::new(SourceVirtual(format!("library-{fields}.libsonnet").into())),
		}
	}

	pub fn evaluate(&self, cache: Option<&PreparedImportCache>, environment: u32) {
		let mut builder = State::builder();
		builder.import_resolver(self.clone());
		builder.context_initializer(Environment(environment));
		if let Some(cache) = cache {
			builder.prepared_import_cache(cache.clone());
		}
		{
			let state = builder.build();
			let _guard = state.enter();
			let value = state.import_resolved(self.path.clone()).unwrap();
			black_box(value.manifest(JsonFormat::default()).unwrap());
		}
		let _ = jrsonnet_gcmodule::collect_thread_cycles();
	}

	pub fn batch(&self, environments: u32, cached: bool) {
		// A fresh cache includes the first environment's compilation cost.
		let cache = cached.then(PreparedImportCache::default);
		for environment in 0..environments {
			self.evaluate(cache.as_ref(), environment);
		}
	}
}

impl ImportResolver for Library {
	fn load_file_contents(&self, _resolved: &SourcePath) -> jrsonnet_evaluator::Result<Vec<u8>> {
		Ok((*self.code).clone())
	}
}

#[derive(Acyclic)]
struct Environment(u32);

impl ContextInitializer for Environment {
	fn populate(&self, _source: Source, builder: &mut InitialContextBuilder) {
		builder.bind("environment", Thunk::evaluated(Val::Num(self.0.into())));
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}
