//! Status command handler.

use std::{cmp::Ordering, io::Write, path::PathBuf};

use anyhow::Result;
use clap::Args;
use rtk_spec::canonical::EnvironmentSpec;

use super::common::{create_tokio_runtime, engine, evaluate_manifests, get_or_create_connection};
use crate::k8s::{client::ClusterConnection, diff::DiffStrategy};

#[derive(Args)]
pub struct StatusArgs {
	/// Path to check status
	pub path: PathBuf,

	/// String that only a single inline environment contains in its name
	#[arg(long)]
	pub name: Option<String>,

	/// Regex filter on '<kind>/<name>'. See https://tanka.dev/output-filtering
	#[arg(short = 't', long)]
	pub target: Vec<String>,

	#[command(flatten)]
	pub jsonnet: super::JsonnetArgs,
}

/// Run the status command.
pub fn run<W: Write>(args: StatusArgs, writer: W) -> Result<()> {
	let runtime = create_tokio_runtime()?;
	runtime.block_on(run_async(args, writer))
}

async fn run_async<W: Write>(args: StatusArgs, writer: W) -> Result<()> {
	let evaluator = engine(args.jsonnet.into_options());
	// tk accepts --target on status but never applies it.
	let evaluated = evaluate_manifests(&evaluator, &args.path, args.name.as_deref(), &[])?;
	let connection = get_or_create_connection(None, evaluated.spec.as_ref()).await?;
	let mut spec = evaluated.spec;
	if let Some(spec) = &mut spec {
		if spec.diff_strategy.is_none() {
			let strategy = DiffStrategy::from_spec(
				spec,
				connection.server_version(),
				spec.apply_strategy.as_deref(),
			)?;
			spec.diff_strategy = Some(strategy.to_string().into());
		}
	}
	write_status(writer, &connection, spec.as_ref(), evaluated.manifests)
}

fn write_status<W: Write>(
	writer: W,
	connection: &ClusterConnection,
	spec: Option<&EnvironmentSpec>,
	manifests: Vec<serde_json::Value>,
) -> Result<()> {
	write_status_parts(
		writer,
		connection.context_name(),
		connection.cluster_name(),
		spec,
		manifests,
	)
}

fn write_status_parts<W: Write>(
	mut writer: W,
	context: &str,
	cluster: &str,
	spec: Option<&EnvironmentSpec>,
	mut manifests: Vec<serde_json::Value>,
) -> Result<()> {
	writeln!(writer, "Context: {context}")?;
	writeln!(writer, "Cluster: {cluster}")?;
	writeln!(writer, "Environment:")?;
	write_spec(&mut writer, spec.cloned().unwrap_or_default())?;

	manifests.sort_by(compare_manifests);
	writeln!(writer, "Resources:")?;
	let mut table = tabwriter::TabWriter::new(writer).padding(4);
	writeln!(table, "  NAMESPACE\tOBJECTSPEC")?;
	for manifest in manifests {
		let namespace = manifest
			.pointer("/metadata/namespace")
			.and_then(serde_json::Value::as_str)
			.unwrap_or_default();
		let kind = manifest
			.get("kind")
			.and_then(serde_json::Value::as_str)
			.unwrap_or_default();
		let name = manifest
			.pointer("/metadata/name")
			.and_then(serde_json::Value::as_str)
			.unwrap_or_default();
		writeln!(table, "  {namespace}\t{kind}/{name}")?;
	}
	table.flush()?;
	Ok(())
}

fn write_spec<W: Write>(writer: &mut W, spec: EnvironmentSpec) -> Result<()> {
	let serialized = serde_json::to_value(&spec)?;
	let field =
		|name: &str, default: serde_json::Value| serialized.get(name).cloned().unwrap_or(default);
	let mut expect_versions = field("expectVersions", serde_json::json!({}));
	if let Some(expect_versions) = expect_versions.as_object_mut() {
		expect_versions.entry("tanka").or_insert_with(|| "".into());
	}
	let mut resource_defaults = field("resourceDefaults", serde_json::json!({}));
	if let Some(resource_defaults) = resource_defaults.as_object_mut() {
		resource_defaults
			.entry("annotations")
			.or_insert_with(|| serde_json::json!({}));
		resource_defaults
			.entry("labels")
			.or_insert_with(|| serde_json::json!({}));
	}
	for (name, value, struct_value) in [
		("APIServer", field("apiServer", "".into()), false),
		("ApplyStrategy", field("applyStrategy", "".into()), false),
		(
			"ContextNames",
			field("contextNames", Vec::<String>::new().into()),
			false,
		),
		("DiffStrategy", field("diffStrategy", "".into()), false),
		("ExpectVersions", expect_versions, true),
		(
			"ExportJsonnetImplementation",
			field("exportJsonnetImplementation", "".into()),
			true,
		),
		("InjectLabels", field("injectLabels", false.into()), false),
		("Namespace", field("namespace", "default".into()), false),
		("ResourceDefaults", resource_defaults, true),
		(
			"TankaEnvLabelFromFields",
			field("tankaEnvLabelFromFields", Vec::<String>::new().into()),
			false,
		),
	] {
		writeln!(writer, "  {name}: {}", go_value(&value, struct_value))?;
	}
	Ok(())
}

fn go_value(value: &serde_json::Value, struct_fields: bool) -> String {
	match value {
		serde_json::Value::Null => "<nil>".to_owned(),
		serde_json::Value::Bool(value) => value.to_string(),
		serde_json::Value::Number(value) => value.to_string(),
		serde_json::Value::String(value) => value.clone(),
		serde_json::Value::Array(values) => format!(
			"[{}]",
			values
				.iter()
				.map(|value| go_value(value, false))
				.collect::<Vec<_>>()
				.join(" ")
		),
		serde_json::Value::Object(values) => {
			let mut values: Vec<_> = values.iter().collect();
			values.sort_by_key(|(name, _)| {
				if struct_fields {
					go_field_name(name)
				} else {
					(*name).clone()
				}
			});
			let fields = values
				.into_iter()
				.map(|(name, value)| {
					let name = if struct_fields {
						go_field_name(name)
					} else {
						name.clone()
					};
					format!("{name}:{}", go_value(value, false))
				})
				.collect::<Vec<_>>()
				.join(" ");
			format!("map[{fields}]")
		}
	}
}

fn go_field_name(name: &str) -> String {
	match name {
		"apiServer" => "APIServer".to_owned(),
		"tanka" => "Tanka".to_owned(),
		_ => {
			let mut chars = name.chars();
			chars
				.next()
				.map(|first| first.to_uppercase().chain(chars).collect())
				.unwrap_or_default()
		}
	}
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
	fn renders_tanka_status_layout_and_resource_order() {
		let spec: EnvironmentSpec = serde_json::from_value(serde_json::json!({
			"apiServer": "https://cluster.example",
			"contextNames": ["dev"],
			"namespace": "monitoring",
			"injectLabels": true,
			"expectVersions": { "tanka": ">=0.30" },
			"resourceDefaults": { "labels": { "app": "api" } }
		}))
		.unwrap();
		let manifests = vec![
			serde_json::json!({
				"apiVersion": "apps/v1",
				"kind": "Deployment",
				"metadata": { "name": "api", "namespace": "monitoring" }
			}),
			serde_json::json!({
				"apiVersion": "v1",
				"kind": "ConfigMap",
				"metadata": { "name": "config", "namespace": "monitoring" }
			}),
		];
		let mut output = Vec::new();
		write_status_parts(&mut output, "dev", "dev-cluster", Some(&spec), manifests).unwrap();
		let output = String::from_utf8(output).unwrap();

		assert!(output.starts_with(
			"Context: dev\nCluster: dev-cluster\nEnvironment:\n  APIServer: https://cluster.example\n"
		));
		assert!(output.contains("  ExpectVersions: map[Tanka:>=0.30]\n"));
		assert!(output.contains("  ResourceDefaults: map[Annotations:map[] Labels:map[app:api]]\n"));
		assert!(output.find("ConfigMap/config").unwrap() < output.find("Deployment/api").unwrap());
	}
}
