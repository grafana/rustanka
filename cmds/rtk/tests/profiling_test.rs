use std::process::Command;

#[test]
fn exporter_profiling_configuration_works_in_fresh_processes() {
	let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("../../test_fixtures/golden_envs/number_formatting_env");
	for enabled in [Some("false"), Some("true"), None] {
		let output_dir = tempfile::tempdir().unwrap();
		let mut command = Command::new(env!("CARGO_BIN_EXE_rtk"));
		command
			.current_dir(&fixture)
			.args(["--log-level=debug", "export"])
			.arg(output_dir.path())
			.arg(".")
			.env_remove("PYROSCOPE_ENABLED")
			.env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
			.env("PYROSCOPE_BASIC_AUTH_USER", "test-user")
			.env("PYROSCOPE_BASIC_AUTH_PASSWORD", "test-token")
			.env(
				"PYROSCOPE_URL",
				if enabled == Some("false") {
					""
				} else {
					"http://127.0.0.1:1"
				},
			);
		if let Some(enabled) = enabled {
			command.env("PYROSCOPE_ENABLED", enabled);
		}
		let output = command.output().unwrap();
		let stderr = String::from_utf8_lossy(&output.stderr);
		assert!(output.status.success(), "enabled={enabled:?}: {stderr}");
		assert!(!stderr.contains("panicked"), "{stderr}");
		assert_eq!(
			stderr.contains("starting cpu profiler"),
			enabled != Some("false"),
			"enabled={enabled:?}: {stderr}",
		);
		let manifest = std::fs::read_to_string(
			output_dir
				.path()
				.join("example.com-v1.NumberFormatting-numbers.yaml"),
		)
		.unwrap();
		assert!(manifest.contains("activeSeriesLimit: 2.6666666666666668e+07"));
	}
}
