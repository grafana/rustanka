use rtk_jsonnet_core as jsonnet;

mod functions;

#[derive(Clone, Copy, Debug, Default)]
pub struct Plugin;

impl Plugin {
	pub fn new() -> Plugin {
		Plugin
	}
}

impl<'a, E> jsonnet::Plugin<'a, E> for Plugin
where
	E: jsonnet::Evaluator<'a>,
{
	fn install(
		self,
		evaluator: &mut E,
	) -> Result<(), <<E as jsonnet::Evaluator<'a>>::Implementation as jsonnet::Implementation>::Error>
	{
		evaluator.with_native_function("parseJson", functions::parse_json::Function)?;
		evaluator.with_native_function("parseYaml", functions::parse_yaml::Function)?;
		evaluator.with_native_function(
			"manifestJsonFromJson",
			functions::manifest_json_from_json::Function,
		)?;
		evaluator.with_native_function(
			"manifestYamlFromJson",
			functions::manifest_yaml_from_json::Function,
		)?;
		evaluator.with_native_function("sha256", functions::sha256::Function)?;
		Ok(())
	}
}
