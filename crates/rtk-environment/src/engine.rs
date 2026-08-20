use std::fmt::Write;

#[derive(Clone, Debug)]
pub struct Engine {
	pub(crate) jsonnet: rtk_jsonnet::Engine,
}

/// Exporting environments in parallel hands a clone of the engine to every
/// worker, so this must hold. Evaluators and evaluated values are `Rc`-based
/// and therefore stay on the thread that created them, but the engine itself is
/// just an `Arc` around configuration and lazily initialized implementations.
const _: () = {
	const fn assert_send_sync<T: Send + Sync>() {}
	assert_send_sync::<Engine>();
};

impl Engine {
	pub fn new(engine: rtk_jsonnet::Engine) -> Engine {
		Engine { jsonnet: engine }
	}

	/// A snippet that imports an entrypoint as `main`, for `script` to work on.
	///
	/// An entrypoint taking top level arguments imports as a function rather than
	/// as what it builds, so it has to be called. The arguments reach it through a
	/// wrapping function for the evaluator to apply them to, and every parameter
	/// is given a default because the evaluator passes only the arguments it was
	/// actually given.
	pub(crate) fn entrypoint_snippet(&self, entrypoint: &str, script: &str) -> String {
		let options = self.jsonnet.options();
		if !options.has_top_level_args() {
			return format!(r#"local main = import "{entrypoint}"; {script}"#);
		}

		let count = options.top_level_arguments.len() + options.top_level_code.len();
		let mut arguments = String::with_capacity(count * 16);
		let mut parameters = String::with_capacity(count * 24);

		let names = options
			.top_level_arguments
			.keys()
			.chain(options.top_level_code.keys());

		for (index, name) in names.enumerate() {
			if index != 0 {
				arguments.push_str(", ");
				parameters.push_str(", ");
			}

			arguments.push_str(name);
			let _ = write!(&mut parameters, "{name} = null");
		}

		format!(
			r#"function({parameters})
				local main = (import "{entrypoint}")({arguments});
				{script}"#
		)
	}
}
