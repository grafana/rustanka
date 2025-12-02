use anyhow::Result;
use clap::{Parser, Subcommand};

mod discover;
mod env;
mod eval;
mod export;
mod jpath;
mod spec;

#[derive(Parser)]
#[command(name = "rtk")]
#[command(about = "Tanka dummy CLI", long_about = None)]
#[command(version)]
struct Cli {
	#[command(subcommand)]
	command: Commands,
}

#[derive(Subcommand)]
enum Commands {
	/// Apply the configuration to the cluster
	Apply {
		/// Path to apply
		path: String,

		#[arg(long)]
		apply_strategy: Option<String>,

		#[arg(long)]
		auto_approve: Option<String>,

		#[arg(long, default_value = "auto")]
		color: String,

		#[arg(long)]
		diff_strategy: Option<String>,

		#[arg(long)]
		dry_run: Option<String>,

		#[arg(long)]
		ext_code: Vec<String>,

		#[arg(short = 'V', long)]
		ext_str: Vec<String>,

		#[arg(long)]
		force: bool,

		#[arg(long, default_value = "go")]
		jsonnet_implementation: String,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		max_stack: Option<i32>,

		#[arg(long)]
		name: Option<String>,

		#[arg(short = 't', long)]
		target: Vec<String>,

		#[arg(long)]
		tla_code: Vec<String>,

		#[arg(short = 'A', long)]
		tla_str: Vec<String>,

		#[arg(long, default_value = "true")]
		validate: bool,
	},

	/// Jsonnet as yaml
	Show {
		/// Path to show
		path: String,

		#[arg(long)]
		dangerous_allow_redirect: bool,

		#[arg(long)]
		ext_code: Vec<String>,

		#[arg(short = 'V', long)]
		ext_str: Vec<String>,

		#[arg(long, default_value = "go")]
		jsonnet_implementation: String,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		max_stack: Option<i32>,

		#[arg(long)]
		name: Option<String>,

		#[arg(short = 't', long)]
		target: Vec<String>,

		#[arg(long)]
		tla_code: Vec<String>,

		#[arg(short = 'A', long)]
		tla_str: Vec<String>,
	},

	/// Differences between the configuration and the cluster
	Diff {
		/// Path to diff
		path: String,

		#[arg(long, default_value = "auto")]
		color: String,

		#[arg(long)]
		diff_strategy: Option<String>,

		#[arg(short = 'z', long)]
		exit_zero: bool,

		#[arg(long)]
		ext_code: Vec<String>,

		#[arg(short = 'V', long)]
		ext_str: Vec<String>,

		#[arg(long, default_value = "go")]
		jsonnet_implementation: String,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		max_stack: Option<i32>,

		#[arg(long)]
		name: Option<String>,

		#[arg(short = 's', long)]
		summarize: bool,

		#[arg(short = 't', long)]
		target: Vec<String>,

		#[arg(long)]
		tla_code: Vec<String>,

		#[arg(short = 'A', long)]
		tla_str: Vec<String>,

		#[arg(short = 'p', long)]
		with_prune: bool,
	},

	/// Delete resources removed from Jsonnet
	Prune {
		/// Path to prune
		path: String,

		#[arg(long)]
		auto_approve: Option<String>,

		#[arg(long, default_value = "auto")]
		color: String,

		#[arg(long)]
		dry_run: Option<String>,

		#[arg(long)]
		ext_code: Vec<String>,

		#[arg(short = 'V', long)]
		ext_str: Vec<String>,

		#[arg(long)]
		force: bool,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		max_stack: Option<i32>,

		#[arg(long)]
		name: Option<String>,

		#[arg(long)]
		tla_code: Vec<String>,

		#[arg(short = 'A', long)]
		tla_str: Vec<String>,
	},

	/// Delete the environment from cluster
	Delete {
		/// Path to delete
		path: String,

		#[arg(long)]
		auto_approve: Option<String>,

		#[arg(long, default_value = "auto")]
		color: String,

		#[arg(long)]
		dry_run: Option<String>,

		#[arg(long)]
		ext_code: Vec<String>,

		#[arg(short = 'V', long)]
		ext_str: Vec<String>,

		#[arg(long)]
		force: bool,

		#[arg(long, default_value = "go")]
		jsonnet_implementation: String,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		max_stack: Option<i32>,

		#[arg(long)]
		name: Option<String>,

		#[arg(short = 't', long)]
		target: Vec<String>,

		#[arg(long)]
		tla_code: Vec<String>,

		#[arg(short = 'A', long)]
		tla_str: Vec<String>,
	},

	/// Manipulate environments
	Env {
		#[command(subcommand)]
		command: EnvCommands,

		#[arg(long, default_value = "info")]
		log_level: String,
	},

	/// Display an overview of the environment, including contents and metadata
	Status {
		/// Path to check status
		path: String,

		#[arg(long)]
		ext_code: Vec<String>,

		#[arg(short = 'V', long)]
		ext_str: Vec<String>,

		#[arg(long, default_value = "go")]
		jsonnet_implementation: String,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		max_stack: Option<i32>,

		#[arg(long)]
		name: Option<String>,

		#[arg(short = 't', long)]
		target: Vec<String>,

		#[arg(long)]
		tla_code: Vec<String>,

		#[arg(short = 'A', long)]
		tla_str: Vec<String>,
	},

	/// Export environments found in path(s)
	Export {
		/// Output directory
		output_dir: String,

		/// Paths to export
		paths: Vec<String>,

		#[arg(short = 'e', long)]
		cache_envs: Vec<String>,

		#[arg(short = 'c', long)]
		cache_path: Option<String>,

		#[arg(long)]
		ext_code: Vec<String>,

		#[arg(short = 'V', long)]
		ext_str: Vec<String>,

		#[arg(long, default_value = "yaml")]
		extension: String,

		#[arg(
			long,
			default_value = "{{.apiVersion}}.{{.kind}}-{{or .metadata.name .metadata.generateName}}"
		)]
		format: String,

		#[arg(long, default_value = "go")]
		jsonnet_implementation: String,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		max_stack: Option<i32>,

		#[arg(long)]
		mem_ballast_size_bytes: Option<i32>,

		#[arg(long)]
		merge_deleted_envs: Vec<String>,

		#[arg(long)]
		merge_strategy: Option<String>,

		#[arg(long)]
		name: Option<String>,

		#[arg(short = 'p', long, default_value = "8")]
		parallel: i32,

		#[arg(short = 'r', long)]
		recursive: bool,

		#[arg(short = 'l', long)]
		selector: Option<String>,

		#[arg(short = 't', long)]
		target: Vec<String>,

		#[arg(long)]
		tla_code: Vec<String>,

		#[arg(short = 'A', long)]
		tla_str: Vec<String>,
	},

	/// Format Jsonnet code
	Fmt {
		/// Files or directories to format
		paths: Vec<String>,

		#[arg(short = 'e', long, default_values_t = vec!["**/.*".to_string(), ".*".to_string(), "**/vendor/**".to_string(), "vendor/**".to_string()])]
		exclude: Vec<String>,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		stdout: bool,

		#[arg(short = 't', long)]
		test: bool,

		#[arg(short = 'v', long)]
		verbose: bool,
	},

	/// Lint Jsonnet code
	Lint {
		/// Files or directories to lint
		paths: Vec<String>,

		#[arg(short = 'e', long, default_values_t = vec!["**/.*".to_string(), ".*".to_string(), "**/vendor/**".to_string(), "vendor/**".to_string()])]
		exclude: Vec<String>,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(short = 'n', long, default_value = "4")]
		parallelism: i32,
	},

	/// Evaluate the jsonnet to json
	Eval {
		/// Path to evaluate
		path: String,

		#[arg(short = 'e', long)]
		eval: Option<String>,

		#[arg(long)]
		ext_code: Vec<String>,

		#[arg(short = 'V', long)]
		ext_str: Vec<String>,

		#[arg(long, default_value = "go")]
		jsonnet_implementation: String,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		max_stack: Option<i32>,

		#[arg(long)]
		tla_code: Vec<String>,

		#[arg(short = 'A', long)]
		tla_str: Vec<String>,
	},

	/// Create the directory structure
	Init {
		#[arg(short = 'f', long)]
		force: bool,

		#[arg(short = 'i', long)]
		inline: bool,

		#[arg(long, default_value = "1.29")]
		k8s: String,

		#[arg(long, default_value = "info")]
		log_level: String,
	},

	/// Handy utilities for working with jsonnet
	Tool {
		#[command(subcommand)]
		command: ToolCommands,

		#[arg(long, default_value = "info")]
		log_level: String,
	},

	/// Install CLI completions
	Complete {
		#[arg(long)]
		remove: bool,
	},
}

#[derive(Subcommand)]
enum EnvCommands {
	/// List environments relative to current dir or <path>
	List {
		/// Path to search for environments
		path: Option<String>,

		#[arg(long)]
		ext_code: Vec<String>,

		#[arg(short = 'V', long)]
		ext_str: Vec<String>,

		#[arg(long)]
		json: bool,

		#[arg(long, default_value = "go")]
		jsonnet_implementation: String,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		max_stack: Option<i32>,

		#[arg(long)]
		names: bool,

		#[arg(short = 'l', long)]
		selector: Option<String>,

		#[arg(long)]
		tla_code: Vec<String>,

		#[arg(short = 'A', long)]
		tla_str: Vec<String>,
	},
}

#[derive(Subcommand)]
enum ToolCommands {
	/// Export JSONNET_PATH for use with other jsonnet tools
	Jpath {
		/// File or directory
		path: String,

		#[arg(short = 'd', long)]
		debug: bool,

		#[arg(long, default_value = "info")]
		log_level: String,
	},

	/// List all transitive imports of an environment
	Imports {
		/// Path to check imports
		path: String,

		#[arg(short = 'c', long)]
		check: Option<String>,

		#[arg(long, default_value = "info")]
		log_level: String,
	},

	/// List all environments that either directly or transitively import the given files
	Importers {
		/// Files to check
		files: Vec<String>,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long, default_value = ".")]
		root: String,
	},

	/// Declarative vendoring of Helm Charts
	Charts {
		#[command(subcommand)]
		command: ChartsCommands,

		#[arg(long, default_value = "info")]
		log_level: String,
	},
}

#[derive(Subcommand)]
enum ChartsCommands {
	/// Create a new Chartfile
	Init {
		#[arg(long, default_value = "info")]
		log_level: String,
	},

	/// Adds Charts to the chartfile
	Add {
		/// Charts to add (format: chart@version)
		charts: Vec<String>,

		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		repository_config: Option<String>,
	},

	/// Adds a repository to the chartfile
	AddRepo {
		/// Repository name
		name: String,

		/// Repository URL
		url: String,

		#[arg(long, default_value = "info")]
		log_level: String,
	},

	/// Download Charts to a local folder
	Vendor {
		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		prune: bool,

		#[arg(long)]
		repository_config: Option<String>,
	},

	/// Displays the current manifest
	Config {
		#[arg(long, default_value = "info")]
		log_level: String,
	},

	/// Check required charts for updated versions
	VersionCheck {
		#[arg(long, default_value = "info")]
		log_level: String,

		#[arg(long)]
		pretty_print: bool,

		#[arg(long)]
		repository_config: Option<String>,
	},
}

fn main() -> Result<()> {
	let cli = Cli::parse();

	match cli.command {
		Commands::Apply { .. } => {
			anyhow::bail!("not implemented");
		}
		Commands::Show { .. } => {
			anyhow::bail!("not implemented");
		}
		Commands::Diff { .. } => {
			anyhow::bail!("not implemented");
		}
		Commands::Prune { .. } => {
			anyhow::bail!("not implemented");
		}
		Commands::Delete { .. } => {
			anyhow::bail!("not implemented");
		}
		Commands::Env { command, .. } => match command {
			EnvCommands::List { path, json, .. } => {
				env::list_envs(path, json)?;
				Ok(())
			}
		},
		Commands::Status { .. } => {
			anyhow::bail!("not implemented");
		}
		Commands::Export {
			output_dir,
			paths,
			ext_code,
			ext_str,
			extension,
			format,
			max_stack,
			parallel,
			tla_code,
			tla_str,
			..
		} => {
			// Parse ext_code flags
			let mut ext_code_map = std::collections::HashMap::new();
			for item in ext_code {
				if let Some((key, value)) = item.split_once('=') {
					ext_code_map.insert(key.to_string(), value.to_string());
				}
			}

			// Parse ext_str flags
			let mut ext_str_map = std::collections::HashMap::new();
			for item in ext_str {
				if let Some((key, value)) = item.split_once('=') {
					ext_str_map.insert(key.to_string(), value.to_string());
				}
			}

			// Parse tla_code flags
			let mut tla_code_map = std::collections::HashMap::new();
			for item in tla_code {
				if let Some((key, value)) = item.split_once('=') {
					tla_code_map.insert(key.to_string(), value.to_string());
				}
			}

			// Parse tla_str flags
			let mut tla_str_map = std::collections::HashMap::new();
			for item in tla_str {
				if let Some((key, value)) = item.split_once('=') {
					tla_str_map.insert(key.to_string(), value.to_string());
				}
			}

			// Convert format from Go template syntax to handlebars
			// Go: {{.apiVersion}} -> Handlebars: {{apiVersion}}
			let hb_format = format
				.replace("{{.", "{{")
				.replace("{{or ", "{{")
				.replace(" .metadata.generateName}}", "}}");

			let eval_opts = eval::EvalOpts {
				ext_str: ext_str_map,
				ext_code: ext_code_map,
				tla_str: tla_str_map,
				tla_code: tla_code_map,
				max_stack: max_stack.map(|s| s as usize),
				eval_expr: None,
			};

			let export_opts = export::ExportOpts {
				output_dir: std::path::PathBuf::from(&output_dir),
				extension,
				format: hb_format,
				parallelism: parallel as usize,
				eval_opts,
			};

			let result = export::export(&paths, export_opts)?;

			println!(
				"Exported {} environments ({} successful, {} failed)",
				result.total_envs, result.successful, result.failed
			);

			for env_result in &result.results {
				if let Some(ref error) = env_result.error {
					eprintln!("  ✗ {:?}: {}", env_result.env_path, error);
				} else {
					println!(
						"  ✓ {:?}: {} files",
						env_result.env_path,
						env_result.files_written.len()
					);
				}
			}

			if result.failed > 0 {
				anyhow::bail!("{} environments failed to export", result.failed);
			}

			Ok(())
		}
		Commands::Fmt { .. } => {
			anyhow::bail!("not implemented");
		}
		Commands::Lint { .. } => {
			anyhow::bail!("not implemented");
		}
		Commands::Eval {
			path,
			eval: eval_expr,
			ext_code,
			ext_str,
			tla_code,
			tla_str,
			max_stack,
			..
		} => {
			// Parse ext_code flags (format: key=value)
			let mut ext_code_map = std::collections::HashMap::new();
			for item in ext_code {
				if let Some((key, value)) = item.split_once('=') {
					ext_code_map.insert(key.to_string(), value.to_string());
				}
			}

			// Parse ext_str flags
			let mut ext_str_map = std::collections::HashMap::new();
			for item in ext_str {
				if let Some((key, value)) = item.split_once('=') {
					ext_str_map.insert(key.to_string(), value.to_string());
				}
			}

			// Parse tla_code flags
			let mut tla_code_map = std::collections::HashMap::new();
			for item in tla_code {
				if let Some((key, value)) = item.split_once('=') {
					tla_code_map.insert(key.to_string(), value.to_string());
				}
			}

			// Parse tla_str flags
			let mut tla_str_map = std::collections::HashMap::new();
			for item in tla_str {
				if let Some((key, value)) = item.split_once('=') {
					tla_str_map.insert(key.to_string(), value.to_string());
				}
			}

			let opts = eval::EvalOpts {
				ext_str: ext_str_map,
				ext_code: ext_code_map,
				tla_str: tla_str_map,
				tla_code: tla_code_map,
				max_stack: max_stack.map(|s| s as usize),
				eval_expr,
			};

			let result = eval::eval(&path, opts)?;

			// Pretty print the result
			let output = serde_json::to_string_pretty(&result.value)?;
			println!("{}", output);

			Ok(())
		}
		Commands::Init { .. } => {
			anyhow::bail!("not implemented");
		}
		Commands::Tool { command, .. } => match command {
			ToolCommands::Jpath { .. } => {
				anyhow::bail!("not implemented");
			}
			ToolCommands::Imports { .. } => {
				anyhow::bail!("not implemented");
			}
			ToolCommands::Importers { .. } => {
				anyhow::bail!("not implemented");
			}
			ToolCommands::Charts { command, .. } => match command {
				ChartsCommands::Init { .. } => {
					anyhow::bail!("not implemented");
				}
				ChartsCommands::Add { .. } => {
					anyhow::bail!("not implemented");
				}
				ChartsCommands::AddRepo { .. } => {
					anyhow::bail!("not implemented");
				}
				ChartsCommands::Vendor { .. } => {
					anyhow::bail!("not implemented");
				}
				ChartsCommands::Config { .. } => {
					anyhow::bail!("not implemented");
				}
				ChartsCommands::VersionCheck { .. } => {
					anyhow::bail!("not implemented");
				}
			},
		},
		Commands::Complete { .. } => {
			anyhow::bail!("not implemented");
		}
	}
}
