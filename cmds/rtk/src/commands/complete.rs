//! Complete command handler.

use std::{
	ffi::OsStr,
	fs,
	io::Write,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Args;
use clap_complete::engine::CompletionCandidate;
use walkdir::{DirEntry, WalkDir};

#[derive(Args)]
pub struct CompleteArgs {
	#[arg(long)]
	pub remove: bool,
}

/// Run the complete command.
pub fn run<W: Write>(args: CompleteArgs, _writer: W) -> Result<()> {
	let home = dirs::home_dir().context("finding home directory")?;
	let config_home = std::env::var_os("XDG_CONFIG_HOME")
		.map(PathBuf::from)
		.unwrap_or_else(|| home.join(".config"));
	let data_home = std::env::var_os("XDG_DATA_HOME")
		.map(PathBuf::from)
		.unwrap_or_else(|| home.join(".local/share"));
	update_completions(&home, &config_home, &data_home, args.remove)
}

#[derive(Clone, Copy)]
enum Shell {
	Bash,
	Zsh,
	Fish,
}

struct Installation {
	shell: Shell,
	rc: Option<PathBuf>,
	script: PathBuf,
}

fn update_completions(
	home: &Path,
	config_home: &Path,
	data_home: &Path,
	remove: bool,
) -> Result<()> {
	let installations = installations(home, config_home, data_home);
	if installations.is_empty() {
		anyhow::bail!(
			"Did not find any shells to {}",
			if remove { "uninstall" } else { "install" }
		);
	}
	let mut errors = Vec::new();
	for installation in installations {
		let result = if remove {
			uninstall(&installation)
		} else {
			install(&installation)
		};
		if let Err(error) = result {
			errors.push(error.to_string());
		}
	}
	if !errors.is_empty() {
		anyhow::bail!(errors.join("\n"));
	}
	Ok(())
}

fn installations(home: &Path, config_home: &Path, data_home: &Path) -> Vec<Installation> {
	let mut found = Vec::new();
	let bash_candidates: &[&str] = if cfg!(target_os = "macos") {
		&[".bash_profile"]
	} else {
		&[".bashrc", ".bash_profile", ".bash_login", ".profile"]
	};
	if let Some(rc) = bash_candidates
		.iter()
		.map(|name| home.join(name))
		.find(|path| path.is_file())
	{
		found.push(Installation {
			shell: Shell::Bash,
			rc: Some(rc),
			script: data_home.join("rtk/completions/rtk.bash"),
		});
	}
	let zshrc = home.join(".zshrc");
	if zshrc.is_file() {
		found.push(Installation {
			shell: Shell::Zsh,
			rc: Some(zshrc),
			script: data_home.join("rtk/completions/_rtk"),
		});
	}
	let fish = config_home.join("fish");
	if fish.is_dir() {
		found.push(Installation {
			shell: Shell::Fish,
			rc: None,
			script: fish.join("completions/rtk.fish"),
		});
	}
	found
}

fn install(installation: &Installation) -> Result<()> {
	let executable = std::env::current_exe().context("finding rtk executable")?;
	let executable = shell_quote(&executable.to_string_lossy());
	let script = match installation.shell {
		Shell::Bash => format!("source <(COMPLETE=bash {executable})\n"),
		Shell::Zsh => format!("source <(COMPLETE=zsh {executable})\n"),
		Shell::Fish => format!("COMPLETE=fish {executable} | source\n"),
	};
	install_registration(installation, &script)
}

fn install_registration(installation: &Installation, script: impl AsRef<[u8]>) -> Result<()> {
	if installation.script.exists() {
		anyhow::bail!("already installed at {}", installation.script.display());
	}
	let rc_update = installation
		.rc
		.as_ref()
		.map(|rc| {
			let line = source_line(&installation.script);
			let mut contents =
				fs::read_to_string(rc).with_context(|| format!("reading {}", rc.display()))?;
			if matches!(installation.shell, Shell::Zsh)
				&& !contents
					.lines()
					.any(|line| line == "autoload -U +X compinit && compinit")
			{
				contents.push_str("\nautoload -U +X compinit && compinit\n");
			}
			contents.push('\n');
			contents.push_str(&line);
			contents.push('\n');
			Ok::<_, anyhow::Error>((rc, contents))
		})
		.transpose()?;
	if let Some(parent) = installation.script.parent() {
		fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
	}
	fs::write(&installation.script, script)
		.with_context(|| format!("writing {}", installation.script.display()))?;

	if let Some((rc, contents)) = rc_update {
		if let Err(error) = fs::write(rc, contents) {
			let _ = fs::remove_file(&installation.script);
			return Err(error).with_context(|| format!("writing {}", rc.display()));
		}
	}
	Ok(())
}

fn uninstall(installation: &Installation) -> Result<()> {
	if !installation.script.exists() {
		anyhow::bail!("not installed at {}", installation.script.display());
	}
	if let Some(rc) = &installation.rc {
		let source = source_line(&installation.script);
		let contents =
			fs::read_to_string(rc).with_context(|| format!("reading {}", rc.display()))?;
		let mut filtered = contents
			.lines()
			.filter(|line| *line != source)
			.collect::<Vec<_>>()
			.join("\n");
		filtered.push('\n');
		fs::write(rc, filtered).with_context(|| format!("writing {}", rc.display()))?;
	}
	fs::remove_file(&installation.script)
		.with_context(|| format!("removing {}", installation.script.display()))?;
	Ok(())
}

fn source_line(path: &Path) -> String {
	format!("source {}", shell_quote(&path.to_string_lossy()))
}

fn shell_quote(value: &str) -> String {
	format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn environment_candidates(current: &OsStr) -> Vec<CompletionCandidate> {
	let Ok(root) = std::env::current_dir() else {
		return Vec::new();
	};
	environment_candidates_in(&root, current)
}

fn environment_candidates_in(root: &Path, current: &OsStr) -> Vec<CompletionCandidate> {
	let Some(prefix) = current.to_str() else {
		return Vec::new();
	};
	let mut candidates = WalkDir::new(root)
		.into_iter()
		.filter_entry(visible_entry)
		.filter_map(std::result::Result::ok)
		.filter(|entry| entry.file_type().is_file() && entry.file_name() == "main.jsonnet")
		.filter_map(|entry| {
			entry
				.path()
				.parent()?
				.strip_prefix(root)
				.ok()
				.map(Path::to_path_buf)
		})
		.map(|path| {
			if path.as_os_str().is_empty() {
				".".to_owned()
			} else {
				path.to_string_lossy().into_owned()
			}
		})
		.filter(|path| path.starts_with(prefix))
		.collect::<Vec<_>>();
	candidates.sort();
	candidates.dedup();
	candidates
		.into_iter()
		.map(CompletionCandidate::new)
		.collect()
}

fn visible_entry(entry: &DirEntry) -> bool {
	entry.depth() == 0
		|| !entry
			.file_name()
			.to_str()
			.is_some_and(|name| name.starts_with('.'))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn installs_and_removes_zsh_completion() {
		let directory = tempfile::tempdir().unwrap();
		let home = directory.path().join("home");
		let config = home.join(".config");
		let data = home.join(".local/share");
		fs::create_dir_all(&home).unwrap();
		fs::write(home.join(".zshrc"), "export EDITOR=vim\n").unwrap();

		let installation = installations(&home, &config, &data).pop().unwrap();
		install_registration(&installation, b"#compdef rtk\n_rtk() {}\n").unwrap();
		let script = installation.script;
		assert!(fs::read_to_string(&script).unwrap().contains("_rtk"));
		assert!(fs::read_to_string(home.join(".zshrc"))
			.unwrap()
			.contains(&source_line(&script)));

		uninstall(&Installation {
			shell: Shell::Zsh,
			rc: Some(home.join(".zshrc")),
			script: script.clone(),
		})
		.unwrap();
		assert!(!script.exists());
		assert!(!fs::read_to_string(home.join(".zshrc"))
			.unwrap()
			.contains(&source_line(&script)));
	}

	#[test]
	fn quotes_shell_paths() {
		assert_eq!(shell_quote("/tmp/a'b"), "'/tmp/a'\\''b'");
	}

	#[test]
	fn completes_environment_paths() {
		let directory = tempfile::tempdir().unwrap();
		fs::create_dir_all(directory.path().join("environments/prod")).unwrap();
		fs::create_dir_all(directory.path().join("environments/dev")).unwrap();
		fs::create_dir_all(directory.path().join(".hidden/env")).unwrap();
		fs::write(
			directory.path().join("environments/prod/main.jsonnet"),
			"{}",
		)
		.unwrap();
		fs::write(directory.path().join("environments/dev/main.jsonnet"), "{}").unwrap();
		fs::write(directory.path().join(".hidden/env/main.jsonnet"), "{}").unwrap();

		let candidates = environment_candidates_in(directory.path(), OsStr::new("environments/p"));
		assert_eq!(candidates.len(), 1);
		assert_eq!(candidates[0].get_value(), OsStr::new("environments/prod"));
	}
}
