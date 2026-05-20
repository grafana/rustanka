//! Validate command — run policy checks against exported Kubernetes manifests.
//!
//! # Overview
//!
//! The validate command lets you write policy rules as Jsonnet files and run them
//! against the YAML manifests produced by `rtk export`. Three subcommands cover the
//! full workflow:
//!
//! | Subcommand | Purpose |
//! |------------|---------|
//! | `lint`     | Check that validation files are syntactically valid and well-formed |
//! | `test`     | Run unit tests for the validation rules themselves |
//! | `manifests`| Run validation rules against real exported manifests |
//!
//! # Directory layout
//!
//! ```text
//! validations/
//! ├── check_labels.jsonnet          # validation rule
//! ├── check_labels_test.jsonnet     # unit tests for the rule above
//! ├── check_replicas.jsonnet
//! ├── check_replicas_test.jsonnet
//! └── helpers.libsonnet             # shared helpers (ignored by the runner)
//! ```
//!
//! * **Validation files** — any `<name>.jsonnet` file (`.libsonnet` files are ignored,
//!   as are `*_test.jsonnet` files).
//! * **Test files** — `<name>_test.jsonnet`, matched to their validation file by name
//!   (e.g. `check_labels_test.jsonnet` tests `check_labels.jsonnet`).
//!
//! # Writing a validation file
//!
//! A validation file must export an object with one or both of:
//!
//! * **`manifestTest(manifest)`** — called once per manifest. Receives a single
//!   Kubernetes manifest (JSON object). Return `null` if it passes, or an error string.
//! * **`namespaceTest(manifests)`** — called once per namespace. Receives an array of
//!   all manifests in that namespace. Return `null` if it passes, or an error string.
//!   Cluster-scoped resources (no namespace) are excluded.
//!
//! Optionally, the object may also define:
//!
//! * **`kinds`** — an array (or set) of Kubernetes kind strings. When present,
//!   `manifestTest` is only invoked for manifests whose `kind` matches one of
//!   the entries. Has no effect on `namespaceTest`.
//!
//! ```jsonnet
//! // check_labels.jsonnet — runs on all manifests
//! {
//!   manifestTest(manifest)::
//!     if std.objectHas(manifest.metadata, 'labels') then
//!       null
//!     else
//!       'manifest %s/%s is missing labels' % [manifest.kind, manifest.metadata.name],
//! }
//! ```
//!
//! ```jsonnet
//! // check_replicas.jsonnet — only runs on Deployments and StatefulSets
//! {
//!   kinds: std.set(['Deployment', 'StatefulSet']),
//!   manifestTest(manifest)::
//!     if manifest.spec.replicas > 0 then null
//!     else 'replicas must be > 0',
//! }
//! ```
//!
//! # Writing a test file
//!
//! A test file evaluates to an array of test-case objects:
//!
//! ```jsonnet
//! // check_labels_test.jsonnet
//! [
//!   {
//!     name: 'manifest with labels passes',
//!     testType: 'manifestTest',
//!     input: {
//!       apiVersion: 'v1',
//!       kind: 'ConfigMap',
//!       metadata: { name: 'test', labels: { app: 'x' } },
//!     },
//!     expectedError: null,  // expect the check to pass
//!   },
//!   {
//!     name: 'manifest without labels fails',
//!     testType: 'manifestTest',
//!     input: {
//!       apiVersion: 'v1',
//!       kind: 'ConfigMap',
//!       metadata: { name: 'test' },
//!     },
//!     expectedError: 'manifest ConfigMap/test is missing labels',
//!   },
//! ]
//! ```
//!
//! Each object must have:
//!
//! | Field           | Type                  | Description |
//! |-----------------|-----------------------|-------------|
//! | `name`          | string                | Human-readable test case name |
//! | `testType`      | `"manifestTest"` or `"namespaceTest"` | Which function to call |
//! | `input`         | object or array       | Data passed to the function |
//! | `expectedError` | `null` or string      | `null` = expect pass; string = expected error |
//!
//! # Usage examples
//!
//! ```bash
//! # 1. Lint validation files (syntax + structure check)
//! rtk validate lint ./validations
//!
//! # 2. Run unit tests for the validation rules
//! rtk validate test ./validations
//!
//! # 3. Run validations against exported manifests
//! rtk validate manifests ./export-output --tests-dir ./validations
//!
//! # 4. Same, but recursively walk subdirectories in the export dir
//! rtk validate manifests ./export-output --recursive --tests-dir ./validations
//! ```

use std::io::Write;

use anyhow::Result;
use clap::{Args, Subcommand};

pub mod common;
pub mod lint;
pub mod manifests;
pub mod test;

#[derive(Args)]
pub struct ValidateArgs {
	#[command(subcommand)]
	pub command: ValidateCommands,
}

#[derive(Subcommand)]
pub enum ValidateCommands {
	/// Validate exported manifests against validation files
	Manifests(manifests::ManifestsArgs),

	/// Check that validation files are valid Jsonnet and define required functions
	Lint(lint::LintArgs),

	/// Run test files (*_test.jsonnet) against their corresponding validation files
	Test(test::TestArgs),
}

/// Run the validate command.
pub fn run<W: Write>(args: ValidateArgs, writer: W) -> Result<()> {
	match args.command {
		ValidateCommands::Manifests(manifests_args) => manifests::run(manifests_args, writer),
		ValidateCommands::Lint(lint_args) => lint::run(lint_args, writer),
		ValidateCommands::Test(test_args) => test::run(test_args, writer),
	}
}
