use anyhow::{bail, Context, Result};
use pyroscope::{
	backend::{pprof_backend, BackendConfig, PprofConfig},
	pyroscope::{PyroscopeAgent, PyroscopeAgentBuilder, PyroscopeAgentRunning},
};
use std::time::Duration;

const SAMPLE_RATE: u32 = 100;
const UPLOAD_INTERVAL: Duration = Duration::from_secs(1);

struct Config {
	url: String,
	auth: Option<(String, String)>,
}

impl Config {
	fn new(
		enabled: Option<String>,
		url: Option<String>,
		user: Option<String>,
		password: Option<String>,
	) -> Result<Option<Self>> {
		let enabled = enabled
			.map(|value| value.parse::<bool>())
			.transpose()
			.context("PYROSCOPE_ENABLED must be true or false")?;
		if enabled == Some(false) {
			return Ok(None);
		}
		let Some(url) = url else {
			if enabled == Some(true) {
				bail!("PYROSCOPE_URL is required when PYROSCOPE_ENABLED is true");
			}
			if user.is_some() || password.is_some() {
				bail!("PYROSCOPE_URL is required when Pyroscope credentials are set");
			}
			return Ok(None);
		};
		if url.is_empty() {
			bail!("PYROSCOPE_URL must not be empty");
		}
		let auth = match (user, password) {
			(Some(user), Some(password)) if !user.is_empty() && !password.is_empty() => {
				Some((user, password))
			}
			(None, None) => None,
			_ => bail!(
				"PYROSCOPE_BASIC_AUTH_USER and PYROSCOPE_BASIC_AUTH_PASSWORD must both be set"
			),
		};
		Ok(Some(Self { url, auth }))
	}
}

pub struct ProfilingGuard(Option<PyroscopeAgent<PyroscopeAgentRunning>>);

pub fn init() -> Result<ProfilingGuard> {
	let config = Config::new(
		std::env::var("PYROSCOPE_ENABLED").ok(),
		std::env::var("PYROSCOPE_URL").ok(),
		std::env::var("PYROSCOPE_BASIC_AUTH_USER").ok(),
		std::env::var("PYROSCOPE_BASIC_AUTH_PASSWORD").ok(),
	)?;
	let Some(config) = config else {
		return Ok(ProfilingGuard(None));
	};

	// Pyroscope's rustls-no-provider client requires an explicitly installed provider.
	// A preinstalled provider wins; install_default leaves it untouched.
	let _ = rustls::crypto::ring::default_provider().install_default();

	let backend = pprof_backend(
		PprofConfig {
			sample_rate: SAMPLE_RATE,
		},
		BackendConfig::default(),
	);
	let mut builder = PyroscopeAgentBuilder::new(
		config.url,
		"rtk",
		SAMPLE_RATE,
		"pyroscope-rs",
		env!("RTK_VERSION"),
		backend,
	)
	.upload_interval(UPLOAD_INTERVAL);
	if let Some((user, password)) = config.auth {
		builder = builder.basic_auth(user, password);
	}
	Ok(ProfilingGuard(Some(builder.build()?.start()?)))
}

impl ProfilingGuard {
	pub fn shutdown(self) -> Result<()> {
		if let Some(agent) = self.0 {
			let agent = agent.stop()?;
			agent.shutdown();
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::Config;

	#[test]
	fn profiling_is_opt_in() {
		assert!(Config::new(None, None, None, None).unwrap().is_none());
	}

	#[test]
	fn disabled_profiling_ignores_inherited_configuration() {
		assert!(Config::new(
			Some("false".into()),
			Some("".into()),
			Some("user".into()),
			None,
		)
		.unwrap()
		.is_none());
	}

	#[test]
	fn enabled_profiling_requires_an_endpoint() {
		assert!(Config::new(Some("true".into()), None, None, None).is_err());
		assert!(Config::new(Some("invalid".into()), None, None, None).is_err());
		assert!(Config::new(
			Some("true".into()),
			Some("http://localhost:4040".into()),
			None,
			None,
		)
		.unwrap()
		.is_some());
	}

	#[test]
	fn credentials_require_an_endpoint_and_a_complete_pair() {
		assert!(Config::new(None, None, Some("user".into()), Some("token".into())).is_err());
		assert!(Config::new(None, Some("".into()), None, None).is_err());
		assert!(Config::new(
			None,
			Some("http://localhost:4040".into()),
			Some("user".into()),
			None
		)
		.is_err());
		assert!(Config::new(
			None,
			Some("http://localhost:4040".into()),
			None,
			Some("token".into())
		)
		.is_err());
		assert!(Config::new(
			None,
			Some("http://localhost:4040".into()),
			Some("".into()),
			Some("token".into())
		)
		.is_err());
	}

	#[test]
	fn accepts_local_and_authenticated_endpoints() {
		let local = Config::new(None, Some("http://localhost:4040".into()), None, None)
			.unwrap()
			.unwrap();
		assert!(local.auth.is_none());
		let cloud = Config::new(
			None,
			Some("https://profiles.example.com".into()),
			Some("user".into()),
			Some("token".into()),
		)
		.unwrap()
		.unwrap();
		assert_eq!(cloud.auth, Some(("user".into(), "token".into())));
	}
}
