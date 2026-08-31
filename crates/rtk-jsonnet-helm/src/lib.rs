use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use rtk_jsonnet_core as jsonnet;

mod cache;
mod functions;

#[derive(Clone, Debug)]
pub struct Plugin {
	state: Arc<State>,
}

pub type CacheDirectoryResolver = fn(&Path) -> Option<PathBuf>;

impl Plugin {
	pub fn new() -> Plugin {
		Plugin {
			state: Arc::new(State::new(None)),
		}
	}

	pub fn with_disk_cache(cache_directory: CacheDirectoryResolver) -> Plugin {
		Plugin {
			state: Arc::new(State::new(Some(cache_directory))),
		}
	}
}

/// The major version of the helm rtk would run, as helm reports it.
///
/// For a project that named the helm it expects in `tkrc.yaml`. Asking helm
/// costs a process launch, so this is only called when a project actually
/// declared an expectation — nothing pays for a check it did not ask for.
pub fn installed_helm_major_version() -> Result<u64, Box<str>> {
	let state = State::new(None);
	let reported = state.ask_helm(&["version", "--template", "{{ .Version }}"])?;
	let digits = reported
		.trim()
		.trim_start_matches('v')
		.split('.')
		.next()
		.unwrap_or_default();
	digits.parse().map_err(|_| {
		format!("could not read a major version out of helm's {reported:?}").into_boxed_str()
	})
}

impl Default for Plugin {
	fn default() -> Self {
		Self::new()
	}
}

impl<E> jsonnet::Plugin<E> for Plugin
where
	E: jsonnet::Evaluator<Context = E> + jsonnet::Context<Evaluator = E>,
{
	fn install(self, evaluator: &mut E) -> Result<(), E::Error> {
		evaluator.with_native_function(
			"helmTemplate",
			functions::template::Function::new(Arc::clone(&self.state)),
		)?;
		Ok(())
	}
}

#[derive(Debug)]
struct State {
	cache: cache::Cache,
	helm_binary: PathBuf,
	helm_identity: OnceLock<Result<Box<[u8]>, Box<str>>>,
	helm_namespace: OnceLock<Result<Box<str>, Box<str>>>,
}

impl State {
	fn new(cache_directory: Option<CacheDirectoryResolver>) -> State {
		State {
			cache: cache::Cache::new(cache_directory),
			helm_binary: env::var_os("RTK_HELM_PATH")
				.map_or_else(|| PathBuf::from("helm"), PathBuf::from),
			helm_identity: OnceLock::new(),
			helm_namespace: OnceLock::new(),
		}
	}

	fn helm_command(&self) -> Command {
		Command::new(&self.helm_binary)
	}

	/// Run helm and take its standard output, or say why not.
	fn ask_helm(&self, arguments: &[&str]) -> Result<String, Box<str>> {
		let output = self
			.helm_command()
			.args(arguments)
			.stdin(Stdio::null())
			.output()
			.map_err(|error| format!("failed to run helm: {error}").into_boxed_str())?;
		if !output.status.success() {
			return Err(format!(
				"helm {} failed: {}",
				arguments.join(" "),
				String::from_utf8_lossy(&output.stderr).trim()
			)
			.into_boxed_str());
		}

		String::from_utf8(output.stdout)
			.map(|text| text.trim().to_owned())
			.map_err(|error| format!("invalid UTF-8 from helm: {error}").into_boxed_str())
	}

	/// Which helm this is, in as much detail as it will give.
	///
	/// All four fields, because a chart can render every one of them through
	/// `.Capabilities.HelmVersion` — `GoVersion` included, so a toolchain bump
	/// with no helm change still counts as a different helm.
	fn helm_identity(&self) -> Option<&[u8]> {
		let identity = self.helm_identity.get_or_init(|| {
			let identity = self
				.ask_helm(&[
					"version",
					"--template",
					"{{ .Version }}|{{ .GitCommit }}|{{ .GitTreeState }}|{{ .GoVersion }}",
				])
				.map(|version| cache::helm_identity(&self.helm_binary, &version));
			if let Err(error) = &identity {
				tracing::warn!(%error, "helm disk cache is unavailable");
			}
			identity
		});
		match identity {
			Ok(identity) => Some(identity),
			Err(_) => None,
		}
	}

	/// The namespace helm resolves when a caller names none.
	///
	/// `helm env HELM_NAMESPACE` reports the same `settings.Namespace()` that
	/// rendering asks for, so this is helm's own answer rather than a second
	/// implementation of client-go's precedence — which reaches `HELM_NAMESPACE`,
	/// the current context in whichever kubeconfig applies, and a service
	/// account token inside a pod.
	fn helm_namespace(&self) -> Option<&str> {
		let namespace = self.helm_namespace.get_or_init(|| {
			let namespace = self
				.ask_helm(&["env", "HELM_NAMESPACE"])
				.map(String::into_boxed_str);
			if let Err(error) = &namespace {
				tracing::warn!(%error, "helm disk cache is unavailable");
			}
			namespace
		});
		match namespace {
			Ok(namespace) => Some(namespace),
			Err(_) => None,
		}
	}
}
