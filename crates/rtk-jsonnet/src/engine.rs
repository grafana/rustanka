use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, Weak};

use rtk_jsonnet_core::{Evaluator as _, FlagsExt, Function, Implementation};
use rtk_jsonnet_jrsonnet::{
	Error as JrsonnetError, Evaluator as JrsonnetEvaluator, Flag as JrsonnetFlag,
	Implementation as JrsonnetImplementation,
};
use rtk_spec::DeepMerge;
use rtk_spec::canonical::{JsonnetImplementation, Rc};
use thiserror::Error;

/// An error returned by one of the various Jsonnet implementations.
#[derive(Debug, Error)]
pub enum Error {
	#[error("a jrsonnet error occurred")]
	Jrsonnet(#[from] JrsonnetError),
}

impl From<Infallible> for Error {
	fn from(_: Infallible) -> Self {
		unreachable!()
	}
}

#[derive(Clone, Debug)]
pub struct Engine(Arc<EngineInternals>);

impl Engine {
	pub fn new(rc: Rc) -> Engine {
		Engine(Arc::new_cyclic(|engine| EngineInternals {
			rc,
			implementations: RwLock::new(Implementations {
				engine: engine.clone(),
				..Default::default()
			}),
		}))
	}

	pub fn create_evaluator(&self) -> Evaluator {
		Evaluator {
			engine: self.0.clone(),
			rc: self.0.rc.clone(),
			evaluator: None,
		}
	}
}

#[derive(Debug)]
struct EngineInternals {
	rc: Rc,
	implementations: RwLock<Implementations>,
}

#[derive(Debug)]
pub struct Evaluator {
	engine: Arc<EngineInternals>,
	rc: Rc,
	evaluator: Option<ImplementationEvaluator>,
}

macro_rules! call_implementation_evaluator_method {
    ($self:ident, $method:ident, $($argument:expr),* $(,)?) => {
        match &mut $self.evaluator {
            Some(ImplementationEvaluator::Jrsonnet(jrsonnet)) => jrsonnet.$method($($argument),*)?,
            None => {
                let implementation = $self.rc.spec.jsonnet_implementation
                    .as_ref()
                    .map(|i| i.implementation())
                    .cloned()
                    .unwrap_or_default();
                match $self.populate_evaluator(implementation)? {
                    ImplementationEvaluator::Jrsonnet(jrsonnet) => jrsonnet.$method($($argument),*)?,
                }
            },
        }
    };
    (@no_insert: $self:ident, $method:ident, $($argument:expr),* $(,)?) => {
        match &mut $self.evaluator {
            Some(ImplementationEvaluator::Jrsonnet(jrsonnet)) => jrsonnet.$method($($argument),*)?,
            None => panic!("attempt to use evaluator before population"),
        }
    };
    (@with: $evaluator:ident, $method:ident, $($argument:expr),* $(,)?) => {
        match $evaluator {
            ImplementationEvaluator::Jrsonnet(jrsonnet) => jrsonnet.$method($($argument),*)?,
        }
    };
}

impl Evaluator {
	pub fn with_rc(&mut self, rc: Rc) -> Result<&mut Self, Error> {
		self.rc.spec.merge_from(rc.spec);
		let implementation = self
			.rc
			.spec
			.jsonnet_implementation
			.as_ref()
			.map(|i| i.implementation())
			.cloned()
			.unwrap_or_default();
		self.populate_evaluator(implementation)?;
		call_implementation_evaluator_method!(@no_insert: self, with_rc, &self.rc);
		Ok(self)
	}

	pub fn with_import_paths(&mut self, import_paths: Vec<PathBuf>) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_import_paths, import_paths);
		Ok(self)
	}

	pub fn with_external_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_external_code, key, value);
		Ok(self)
	}

	pub fn with_native_function<'a, E, F>(
		&mut self,
		key: &'a str,
		function: F,
	) -> Result<&mut Self, Error>
	where
		E: rtk_jsonnet_core::Evaluator<'a>,
		F: 'static + Function<'a, JrsonnetEvaluator>,
	{
		call_implementation_evaluator_method!(self, with_native_function, key, function);
		Ok(self)
	}

	pub fn with_external_variable(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_external_variable, key, value);
		Ok(self)
	}

	pub fn with_top_level_argument(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_top_level_argument, key, value);
		Ok(self)
	}

	pub fn with_top_level_code(&mut self, key: &str, value: &str) -> Result<&mut Self, Error> {
		call_implementation_evaluator_method!(self, with_top_level_code, key, value);
		Ok(self)
	}
}

impl Evaluator {
	fn populate_evaluator(
		&mut self,
		implementation: JsonnetImplementation,
	) -> Result<&mut ImplementationEvaluator, Error> {
		let mut implementations = self
			.engine
			.implementations
			.write()
			.expect("implementations should not be poisoned");

		implementations.maybe_init_implementation(implementation.clone())?;

		// TODO: Fix for multiple implementations.
		let evaluator = match (implementation, &*implementations) {
			(
				JsonnetImplementation::Jrsonnet,
				Implementations {
					jrsonnet: Some(jrsonnet),
					..
				},
			) => self.evaluator.insert(ImplementationEvaluator::Jrsonnet(
				jrsonnet.create_evaluator(),
			)),
			_ => {
				drop(implementations);
				return self.populate_evaluator(JsonnetImplementation::Jrsonnet);
			}
		};

		call_implementation_evaluator_method!(@with: evaluator, with_plugin, rtk_jsonnet_helm::Plugin::new());
		//call_implementation_evaluator_method!(@with: evaluator, with_native_function, KUSTOMIZE GOES HERE);

		Ok(evaluator)
	}
}

#[derive(Debug)]
enum ImplementationEvaluator {
	Jrsonnet(JrsonnetEvaluator),
}

#[derive(Debug, Default)]
struct Implementations {
	engine: Weak<EngineInternals>,
	jrsonnet: Option<JrsonnetImplementation>,
}

impl Implementations {
	pub fn maybe_init_implementation(
		&mut self,
		implementation: JsonnetImplementation,
	) -> Result<(), Error> {
		let Some(engine) = Weak::upgrade(&self.engine) else {
			panic!("attempt to use implementations after engine is dropped");
		};

		match implementation {
			JsonnetImplementation::Reference => {
				tracing::warn!("the `reference` implementation is not implemented");
				Ok(())
			}
			JsonnetImplementation::GoJsonnet => {
				tracing::warn!("the `go-jsonnet` implementation is not implemented");
				Ok(())
			}
			JsonnetImplementation::Jrsonnet if self.jrsonnet.is_none() => {
				let flags = engine
					.rc
					.flags::<JrsonnetFlag>()
					.map_err(JrsonnetError::Flag)?;
				self.jrsonnet = Some(JrsonnetImplementation::new(flags)?);
				Ok(())
			}
			JsonnetImplementation::Jrsonnet => Ok(()),
			JsonnetImplementation::Binary(binary) => {
				tracing::warn!(binary = ?binary, "the `binary:*` implementation is not implemented");
				Ok(())
			}
		}
	}
}
