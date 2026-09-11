//! Delete command handler.

use std::{cmp::Ordering, io::Write, path::PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use rtk_environments::export::serialize_manifest;

use super::{
	common::{
		create_tokio_runtime, engine, evaluate_manifests, get_or_create_connection,
		validate_dry_run, AutoApprove,
	},
	diff::ColorMode,
};
use crate::k8s::{
	apply::ApplyEngine,
	diff::{DiffStatus, DiffStrategy, ResourceDiff},
	discovery::gvk_from_manifest,
	output::DiffOutput,
};

#[derive(Args)]
pub struct DeleteArgs {
	/// Path to delete
	pub path: PathBuf,

	/// Skip interactive approval. Only for automation! Allowed values: 'always', 'never', 'if-no-changes'
	#[arg(long, value_enum)]
	pub auto_approve: Option<AutoApprove>,

	/// Controls color in diff output, must be "auto", "always", or "never"
	#[arg(long, default_value = "auto", value_enum)]
	pub color: ColorMode,

	/// --dry-run parameter to pass down to kubectl, must be "none", "server", or "client"
	#[arg(long)]
	pub dry_run: Option<String>,

	/// Force applying (kubectl apply --force)
	#[arg(long)]
	pub force: bool,

	/// String that only a single inline environment contains in its name
	#[arg(long)]
	pub name: Option<String>,

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

/// Run the delete command.
pub fn run<W: Write>(args: DeleteArgs, writer: W) -> Result<()> {
	validate_dry_run(args.dry_run.as_deref())?;
	let runtime = create_tokio_runtime()?;
	runtime.block_on(run_async(args, writer))
}

async fn run_async<W: Write>(args: DeleteArgs, mut writer: W) -> Result<()> {
	let dry_run = args.dry_run.filter(|value| !value.is_empty());
	let evaluator = engine(args.jsonnet.into_options());
	let mut evaluated =
		evaluate_manifests(&evaluator, &args.path, args.name.as_deref(), &args.target)?;
	evaluated.manifests.sort_by(compare_manifests);

	let connection = get_or_create_connection(None, evaluated.spec.as_ref()).await?;
	let default_namespace = evaluated
		.spec
		.as_ref()
		.map(|spec| spec.namespace().to_owned())
		.unwrap_or_else(|| connection.default_namespace().to_owned());

	if dry_run.is_none() {
		{
			let mut output = DiffOutput::new(&mut writer, args.color, DiffStrategy::Native)?;
			for manifest in &evaluated.manifests {
				output.write_diff(&deletion_diff(manifest)?)?;
			}
		}
		if args.auto_approve.unwrap_or_default() != AutoApprove::Always {
			confirm_delete(
				&mut writer,
				evaluated
					.spec
					.as_ref()
					.map(|spec| spec.namespace())
					.unwrap_or(&default_namespace),
				&connection,
			)?;
		}
	}

	let apply = ApplyEngine::new(
		connection.client().clone(),
		default_namespace,
		false,
		args.force,
	);
	for manifest in evaluated.manifests.iter().rev() {
		let gvk = gvk_from_manifest(manifest).context("manifest missing apiVersion or kind")?;
		let name = manifest
			.pointer("/metadata/name")
			.and_then(serde_json::Value::as_str)
			.context("manifest missing metadata.name")?;
		let namespace = manifest
			.pointer("/metadata/namespace")
			.and_then(serde_json::Value::as_str);
		let not_found = apply
			.delete_resource_with_options(&gvk, name, namespace, dry_run.as_deref(), args.force)
			.await?;
		if let Some(error) = not_found {
			writeln!(writer, "Delete failed: {error}")?;
		} else if let Some(mode) = dry_run.as_deref() {
			write_delete_result(&mut writer, &gvk, name, namespace, mode, args.force)?;
		}
	}
	Ok(())
}

fn confirm_delete<W: Write>(
	writer: &mut W,
	namespace: &str,
	connection: &crate::k8s::client::ClusterConnection,
) -> Result<()> {
	writeln!(
		writer,
		"Deleting from namespace '{namespace}' of cluster '{}' at '{}' using context '{}'.",
		connection.cluster_name(),
		connection.server_url(),
		connection.context_name()
	)?;
	write!(writer, "Please type 'yes' to confirm: ")?;
	writer.flush()?;
	let mut answer = String::new();
	std::io::stdin().read_line(&mut answer)?;
	if answer.trim() != "yes" {
		anyhow::bail!("aborted by user");
	}
	Ok(())
}

fn write_delete_result<W: Write>(
	writer: &mut W,
	gvk: &kube::core::GroupVersionKind,
	name: &str,
	namespace: Option<&str>,
	dry_run: &str,
	force: bool,
) -> Result<()> {
	let resource = if gvk.group.is_empty() {
		gvk.kind.to_ascii_lowercase()
	} else {
		format!("{}.{}", gvk.kind.to_ascii_lowercase(), gvk.group)
	};
	let action = if force { "force deleted" } else { "deleted" };
	let namespace = namespace
		.map(|namespace| format!(" from {namespace} namespace"))
		.unwrap_or_default();
	let dry_run = match dry_run {
		"client" => " (dry run)",
		"server" => " (server dry run)",
		_ => "",
	};
	writeln!(writer, "{resource} \"{name}\" {action}{namespace}{dry_run}")?;
	Ok(())
}

fn deletion_diff(manifest: &serde_json::Value) -> Result<ResourceDiff> {
	let gvk = gvk_from_manifest(manifest).context("manifest missing apiVersion or kind")?;
	let name = manifest
		.pointer("/metadata/name")
		.and_then(serde_json::Value::as_str)
		.context("manifest missing metadata.name")?
		.to_owned();
	let namespace = manifest
		.pointer("/metadata/namespace")
		.and_then(serde_json::Value::as_str)
		.map(str::to_owned);
	Ok(ResourceDiff {
		gvk,
		namespace,
		name,
		status: DiffStatus::Deleted,
		current_yaml: serialize_manifest(manifest)
			.map_err(|error| anyhow::anyhow!(error.report()))?,
		desired_yaml: String::new(),
	})
}

fn compare_manifests(left: &serde_json::Value, right: &serde_json::Value) -> Ordering {
	manifest_sort_key(left).cmp(&manifest_sort_key(right))
}

fn manifest_sort_key(manifest: &serde_json::Value) -> (usize, &str, &str, &str, &str) {
	let kind = manifest
		.get("kind")
		.and_then(serde_json::Value::as_str)
		.unwrap_or_default();
	let namespace = manifest
		.pointer("/metadata/namespace")
		.and_then(serde_json::Value::as_str)
		.unwrap_or_default();
	let name = manifest
		.pointer("/metadata/name")
		.and_then(serde_json::Value::as_str)
		.unwrap_or_default();
	let generated = manifest
		.pointer("/metadata/generateName")
		.and_then(serde_json::Value::as_str)
		.unwrap_or_default();
	(kind_position(kind), kind, namespace, name, generated)
}

fn kind_position(kind: &str) -> usize {
	const ORDER: &[&str] = &[
		"Namespace",
		"NetworkPolicy",
		"ResourceQuota",
		"LimitRange",
		"PodSecurityPolicy",
		"PodDisruptionBudget",
		"ServiceAccount",
		"Secret",
		"ConfigMap",
		"StorageClass",
		"PersistentVolume",
		"PersistentVolumeClaim",
		"CustomResourceDefinition",
		"ClusterRole",
		"ClusterRoleList",
		"ClusterRoleBinding",
		"ClusterRoleBindingList",
		"Role",
		"RoleList",
		"RoleBinding",
		"RoleBindingList",
		"Service",
		"DaemonSet",
		"Pod",
		"ReplicationController",
		"ReplicaSet",
		"Deployment",
		"HorizontalPodAutoscaler",
		"StatefulSet",
		"Job",
		"CronJob",
		"Ingress",
		"APIService",
	];
	ORDER
		.iter()
		.position(|candidate| *candidate == kind)
		.unwrap_or(ORDER.len())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn renders_each_manifest_as_a_deletion() {
		let diff = deletion_diff(&serde_json::json!({
			"apiVersion": "v1",
			"kind": "ConfigMap",
			"metadata": { "name": "settings", "namespace": "monitoring" },
			"data": { "key": "value" }
		}))
		.unwrap();

		assert_eq!(diff.status, DiffStatus::Deleted);
		assert_eq!(diff.name, "settings");
		assert_eq!(diff.namespace.as_deref(), Some("monitoring"));
		assert!(diff
			.unified_diff(DiffStrategy::Native)
			.contains("-kind: ConfigMap"));
	}

	#[test]
	fn sorts_in_install_order_before_reversing_for_delete() {
		let mut manifests = vec![
			serde_json::json!({
				"apiVersion": "apps/v1", "kind": "Deployment", "metadata": { "name": "api" }
			}),
			serde_json::json!({
				"apiVersion": "v1", "kind": "Namespace", "metadata": { "name": "monitoring" }
			}),
		];
		manifests.sort_by(compare_manifests);
		assert_eq!(manifests[0]["kind"], "Namespace");
		assert_eq!(manifests.iter().rev().next().unwrap()["kind"], "Deployment");
	}
}
