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
}

impl State {
	fn new(cache_directory: Option<CacheDirectoryResolver>) -> State {
		State {
			cache: cache::Cache::new(cache_directory),
			helm_binary: env::var_os("RTK_HELM_PATH")
				.map_or_else(|| PathBuf::from("helm"), PathBuf::from),
			helm_identity: OnceLock::new(),
		}
	}

	fn helm_command(&self) -> Command {
		Command::new(&self.helm_binary)
	}

	fn helm_identity(&self) -> Option<&[u8]> {
		let identity = self.helm_identity.get_or_init(|| {
			let identity = (|| {
				let output = self
					.helm_command()
					.args(["version", "--template", "{{ .Version }}|{{ .GitCommit }}"])
					.stdin(Stdio::null())
					.output()
					.map_err(|error| {
						format!("failed to identify helm: {error}").into_boxed_str()
					})?;
				if !output.status.success() {
					return Err(format!(
						"helm version failed: {}",
						String::from_utf8_lossy(&output.stderr).trim()
					)
					.into_boxed_str());
				}
				let version = String::from_utf8(output.stdout).map_err(|error| {
					format!("invalid UTF-8 in helm version: {error}").into_boxed_str()
				})?;
				Ok(cache::helm_identity(&self.helm_binary, version.trim()))
			})();
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
}
