use std::fmt::Write;
use std::path::Path;

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
	pub(crate) fn entrypoint_snippet(&self, entrypoint: &Path, script: &str) -> String {
		let entrypoint = import_literal(entrypoint);
		let options = self.jsonnet.options();
		if !options.has_top_level_args() {
			return format!("local main = import {entrypoint}; {script}");
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
			"function({parameters})
				local main = (import {entrypoint})({arguments});
				{script}"
		)
	}
}

/// An entrypoint as a Jsonnet string literal, quotes included.
///
/// The entrypoint is imported by its absolute path, as Tanka's `evalJsonnet`
/// imports `jpath.Entrypoint`. A relative name would be resolved against the
/// importing file first, and the snippet doing the importing has no file — so
/// the process working directory decides, and a project with an entrypoint of
/// its own at the root captures every environment below it.
///
/// Separators are normalized the way Tanka's `normaliseImportPath` does, so a
/// Windows path is a valid Jsonnet string rather than a pile of escapes. The
/// literal itself is escaped, which Tanka does not bother with; a path
/// containing a quote is unusual but should not produce unparseable Jsonnet.
fn import_literal(entrypoint: &Path) -> String {
	let entrypoint = entrypoint.to_string_lossy().replace('\\', "/");
	serde_json::to_string(&entrypoint).unwrap_or_else(|_| format!("\"{entrypoint}\""))
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::*;

	fn engine() -> Engine {
		Engine::new(rtk_jsonnet::Engine::new(rtk_jsonnet::Options::default()))
	}

	/// The import has to name the entrypoint absolutely. A bare `main.jsonnet`
	/// is resolved against the importing file before any import path, and the
	/// snippet has no file of its own, so the process working directory decides
	/// — which means a project with an entrypoint at its root swallows every
	/// environment below it.
	#[test]
	fn imports_the_entrypoint_by_its_absolute_path() {
		let snippet = engine().entrypoint_snippet(
			Path::new("/projects/demo/environments/foo/main.jsonnet"),
			"main",
		);

		assert_eq!(
			snippet,
			r#"local main = import "/projects/demo/environments/foo/main.jsonnet"; main"#
		);
	}

	#[test]
	fn normalizes_separators_and_escapes_the_literal() {
		let snippet =
			engine().entrypoint_snippet(Path::new(r"C:\projects\demo\main.jsonnet"), "main");
		assert!(
			snippet.contains(r#"import "C:/projects/demo/main.jsonnet""#),
			"{snippet}"
		);

		let snippet =
			engine().entrypoint_snippet(Path::new(r#"/a "quoted" dir/main.jsonnet"#), "main");
		assert!(
			snippet.contains(r#"import "/a \"quoted\" dir/main.jsonnet""#),
			"{snippet}"
		);
	}

	#[test]
	fn an_entrypoint_taking_top_level_arguments_is_called() {
		let mut options = rtk_jsonnet::Options::default();
		options
			.top_level_arguments
			.insert("name".into(), "value".into());
		let engine = Engine::new(rtk_jsonnet::Engine::new(options));

		let snippet = engine.entrypoint_snippet(Path::new("/demo/main.jsonnet"), "main");

		assert!(snippet.contains("function(name = null)"), "{snippet}");
		assert!(
			snippet.contains(r#"(import "/demo/main.jsonnet")(name)"#),
			"{snippet}"
		);
	}
}
