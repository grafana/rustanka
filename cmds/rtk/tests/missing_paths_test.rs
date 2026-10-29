use std::{fs, process::Command};

#[test]
fn nonexistent_discovery_paths_fail_like_tanka() {
	let project = tempfile::Builder::new()
		.prefix("rtk-missing-paths-")
		.tempdir()
		.unwrap();
	fs::write(project.path().join("jsonnetfile.json"), "{}").unwrap();
	for binary in ["tk", env!("CARGO_BIN_EXE_rtk")] {
		for args in [
			vec!["env", "list", "missing"],
			vec!["export", "out", "missing", "--recursive"],
		] {
			let output = Command::new(binary)
				.args(&args)
				.current_dir(project.path())
				.output()
				.unwrap();
			assert!(
				!output.status.success(),
				"{binary} {args:?} unexpectedly succeeded"
			);
			let stderr = String::from_utf8_lossy(&output.stderr);
			assert!(
				stderr.contains("No such file") || stderr.contains("no such file"),
				"{binary}: {stderr}"
			);
			assert!(!project.path().join("out").exists());
		}
	}
}
