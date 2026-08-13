use rtk_jsonnet_core as jsonnet;

mod functions;

#[derive(Clone, Copy, Debug, Default)]
pub struct Plugin;

impl Plugin {
	pub fn new() -> Plugin {
		Plugin
	}
}

impl<E> jsonnet::Plugin<E> for Plugin
where
	E: jsonnet::Evaluator<Context = E> + jsonnet::Context<Evaluator = E>,
{
	fn install(self, evaluator: &mut E) -> Result<(), E::Error> {
		evaluator.with_native_function("kustomizeBuild", functions::build::Function)?;
		Ok(())
	}
}
