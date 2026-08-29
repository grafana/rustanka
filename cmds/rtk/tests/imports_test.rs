#[cfg(unix)]
mod unix {
	use std::{
		env, fs,
		os::unix::fs::PermissionsExt,
		path::PathBuf,
		process::{Command, Output},
	};

	use tempfile::TempDir;

	const COMMIT: &str = "abc123";

	fn repository_root() -> PathBuf {
		PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.parent()
			.unwrap()
			.parent()
			.unwrap()
			.to_path_buf()
	}

	fn run_check(changed_file: &str) -> Output {
		let temp = TempDir::new().unwrap();
		let git = temp.path().join("git");
		fs::write(
			&git,
			format!(
				r#"#!/bin/sh
if [ "$1 $2 $3 $4 $5" = "diff-tree --no-commit-id --name-only -r {COMMIT}" ]; then
  printf '%s\n' '{changed_file}'
elif [ "$1 $2" = "rev-parse --show-toplevel" ]; then
  printf '%s\n' "$RTK_TEST_GIT_ROOT"
else
  exit 2
fi
"#,
			),
		)
		.unwrap();
		let mut permissions = fs::metadata(&git).unwrap().permissions();
		permissions.set_mode(0o755);
		fs::set_permissions(&git, permissions).unwrap();

		let root = repository_root();
		let mut paths = vec![temp.path().to_path_buf()];
		paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));

		Command::new(env!("CARGO_BIN_EXE_rtk"))
			.args([
				"tool",
				"imports",
				"--check",
				COMMIT,
				"cmds/rtk/testdata/importTree",
			])
			.current_dir(&root)
			.env("PATH", env::join_paths(paths).unwrap())
			.env("RTK_TEST_GIT_ROOT", &root)
			.output()
			.unwrap()
	}

	#[test]
	fn check_exits_16_when_an_import_changed() {
		let output = run_check("cmds/rtk/testdata/importTree/trees/apple.jsonnet");

		assert_eq!(output.status.code(), Some(16));
		assert_eq!(
			String::from_utf8(output.stdout).unwrap(),
			format!(
				"Rebuild required. File `cmds/rtk/testdata/importTree` imports `trees/apple.jsonnet`, which has been changed in `{COMMIT}`.\n"
			)
		);
	}

	#[test]
	fn check_exits_0_when_no_import_changed() {
		let output = run_check("README.md");

		assert_eq!(output.status.code(), Some(0));
		assert_eq!(
			String::from_utf8(output.stdout).unwrap(),
			format!(
				"Rebuild not required, because no imported files have been changed in `{COMMIT}`.\n"
			)
		);
	}
}
