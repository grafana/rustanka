//! Integration tests for the prune command using a mock Kubernetes API server.
//!
//! These tests focus on prune-command behavior (dry-run, confirmation, deletion),
//! while prune detection correctness is primarily covered by diff tests with
//! `with_prune=true`.

use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};

#[path = "test_utils.rs"]
mod test_utils;

use k8s_mock::DiscoveryMode;
use rtk::{
	commands::{
		diff::ColorMode,
		prune::{prune_environment, AutoApprove, PruneOpts},
	},
	k8s::{client::ClusterConnection, diff::ResourceDiff},
};

fn fixture_dir(name: &str) -> PathBuf {
	test_utils::diff_fixture_dir(name)
}

async fn setup_connection(
	test_dir: &Path,
	discovery_mode: DiscoveryMode,
) -> (k8s_mock::RunningHttpMockK8sServer, ClusterConnection) {
	let cluster_state = test_utils::load_manifests_from_dir(&test_dir.join("cluster"));
	test_utils::setup_connection_from_cluster_state(cluster_state, discovery_mode, false).await
}

async fn run_prune(
	env_dir: &Path,
	connection: ClusterConnection,
	opts: PruneOpts,
) -> anyhow::Result<Vec<ResourceDiff>> {
	let mut output = Vec::new();
	prune_environment(
		env_dir,
		Some(connection),
		rtk_jsonnet::Options::default(),
		opts,
		&mut output,
	)
	.await
}

fn default_prune_opts() -> PruneOpts {
	PruneOpts {
		auto_approve: AutoApprove::Always,
		color: ColorMode::Never,
		..Default::default()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn prune_dry_run_does_not_delete() {
		let test_dir = fixture_dir("resource_deleted");
		let env_dir = test_dir.join("environment");
		let (_server, connection) = setup_connection(&test_dir, DiscoveryMode::Aggregated).await;

		let mut dry_run_opts = default_prune_opts();
		dry_run_opts.dry_run = Some("client".to_string());

		let dry_run_diffs = run_prune(&env_dir, connection.clone(), dry_run_opts)
			.await
			.expect("dry-run prune should succeed");
		assert_eq!(dry_run_diffs.len(), 1, "dry-run should find one deletion");
		assert_eq!(dry_run_diffs[0].name, "delete-this");

		let delete_diffs = run_prune(&env_dir, connection, default_prune_opts())
			.await
			.expect("real prune after dry-run should succeed");
		assert_eq!(
			delete_diffs.len(),
			1,
			"resource should still exist after dry-run"
		);
		assert_eq!(delete_diffs[0].name, "delete-this");
	}

	#[tokio::test]
	async fn prune_is_idempotent_after_delete() {
		let test_dir = fixture_dir("resource_deleted");
		let env_dir = test_dir.join("environment");
		let (_server, connection) = setup_connection(&test_dir, DiscoveryMode::Legacy).await;

		let first = run_prune(&env_dir, connection.clone(), default_prune_opts())
			.await
			.expect("first prune should succeed");
		assert_eq!(
			first.len(),
			1,
			"first prune should delete exactly one resource"
		);
		assert_eq!(first[0].name, "delete-this");

		let second = run_prune(&env_dir, connection, default_prune_opts())
			.await
			.expect("second prune should succeed");
		assert!(
			second.is_empty(),
			"second prune should have nothing left to delete"
		);
	}

	#[tokio::test]
	async fn prune_auto_approve_never_errors_in_non_interactive_mode() {
		// Close stdin so is_terminal() returns false even when the test
		// runner is invoked from an interactive terminal.
		drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(0) });

		let test_dir = fixture_dir("resource_deleted");
		let env_dir = test_dir.join("environment");
		let (_server, connection) = setup_connection(&test_dir, DiscoveryMode::Aggregated).await;

		let mut opts = default_prune_opts();
		opts.auto_approve = AutoApprove::Never;

		let err = run_prune(&env_dir, connection, opts)
			.await
			.expect_err("non-interactive prune should fail when prompting is required");
		let msg = format!("{err:#}");
		assert!(
			msg.contains("cannot prompt for confirmation in non-interactive mode"),
			"unexpected error message: {msg}"
		);
	}

	#[tokio::test]
	async fn prune_implicit_namespace_no_false_deletion() {
		let test_dir = fixture_dir("implicit_namespace_prune");
		let env_dir = test_dir.join("environment");
		let (_server, connection) = setup_connection(&test_dir, DiscoveryMode::Aggregated).await;

		let diffs = run_prune(&env_dir, connection, default_prune_opts())
			.await
			.expect("prune should succeed");
		assert!(
			diffs.is_empty(),
			"expected no deletions, but found: {:?}",
			diffs
				.iter()
				.map(|d| format!("{}/{}", d.gvk.kind, d.name))
				.collect::<Vec<_>>()
		);
	}

	#[tokio::test]
	async fn prune_namespace_only_finds_resources_in_selected_namespace() {
		let test_dir = fixture_dir("resource_deleted");
		let env_dir = test_dir.join("environment");
		let mut cluster_state = test_utils::load_manifests_from_dir(&test_dir.join("cluster"));
		let mut other_namespace = cluster_state
			.iter()
			.find(|resource| resource["metadata"]["name"] == "delete-this")
			.unwrap()
			.clone();
		other_namespace["metadata"]["name"] = "delete-other".into();
		other_namespace["metadata"]["namespace"] = "other".into();
		cluster_state.push(other_namespace);

		let (server, connection) = test_utils::setup_connection_from_cluster_state(
			cluster_state,
			DiscoveryMode::Aggregated,
			false,
		)
		.await;
		let mut opts = default_prune_opts();
		opts.namespace = Some("other".to_string());
		opts.dry_run = Some("client".to_string());

		let diffs = run_prune(&env_dir, connection, opts)
			.await
			.expect("namespace-scoped prune should succeed");
		assert_eq!(diffs.len(), 1);
		assert_eq!(diffs[0].name, "delete-other");
		assert_eq!(diffs[0].namespace.as_deref(), Some("other"));

		let prune_list_requests: Vec<_> = server
			.http_exchanges()
			.into_iter()
			.filter(|exchange| {
				exchange.method == "GET"
					&& exchange
						.query
						.as_deref()
						.is_some_and(|query| query.contains("labelSelector"))
			})
			.collect();
		assert!(!prune_list_requests.is_empty());
		assert!(
			prune_list_requests
				.iter()
				.all(|exchange| exchange.path.contains("/namespaces/other/")),
			"unexpected prune list paths: {:?}",
			prune_list_requests
				.iter()
				.map(|exchange| &exchange.path)
				.collect::<Vec<_>>()
		);
	}

	#[tokio::test]
	async fn prune_target_limits_api_lists_and_filters_orphans_by_name() {
		let test_dir = fixture_dir("resource_deleted");
		let env_dir = test_dir.join("environment");
		let mut cluster_state = test_utils::load_manifests_from_dir(&test_dir.join("cluster"));
		let mut second_orphan = cluster_state
			.iter()
			.find(|resource| resource["metadata"]["name"] == "delete-this")
			.unwrap()
			.clone();
		second_orphan["metadata"]["name"] = "delete-other".into();
		cluster_state.push(second_orphan);

		let (server, connection) = test_utils::setup_connection_from_cluster_state(
			cluster_state,
			DiscoveryMode::Aggregated,
			false,
		)
		.await;
		let mut opts = default_prune_opts();
		opts.target = vec!["ConfigMap/delete-this".to_string()];
		opts.dry_run = Some("client".to_string());

		let diffs = run_prune(&env_dir, connection, opts)
			.await
			.expect("targeted prune should succeed");
		assert_eq!(diffs.len(), 1);
		assert_eq!(diffs[0].name, "delete-this");

		let prune_list_paths: Vec<_> = server
			.http_exchanges()
			.into_iter()
			.filter(|exchange| {
				exchange.method == "GET"
					&& exchange
						.query
						.as_deref()
						.is_some_and(|query| query.contains("labelSelector"))
			})
			.map(|exchange| exchange.path)
			.collect();
		assert_eq!(prune_list_paths, vec!["/api/v1/configmaps"]);
	}

	#[tokio::test]
	async fn prune_skips_explicitly_ignored_and_controller_owned_resources() {
		let test_dir = fixture_dir("resource_deleted");
		let env_dir = test_dir.join("environment");
		let mut cluster_state = test_utils::load_manifests_from_dir(&test_dir.join("cluster"));
		let orphan = cluster_state
			.iter()
			.find(|resource| resource["metadata"]["name"] == "delete-this")
			.unwrap()
			.clone();
		let mut ignored = orphan.clone();
		ignored["metadata"]["annotations"]["tanka.dev/prune-ignore"] = "true".into();
		let mut owned = orphan.clone();
		owned["metadata"]["name"] = "owned".into();
		owned["metadata"]["ownerReferences"] = serde_json::json!([{"apiVersion":"v1","kind":"ConfigMap","name":"parent","uid":"parent-uid","controller":true}]);
		let mut false_owner = orphan;
		false_owner["metadata"]["name"] = "false-owner".into();
		false_owner["metadata"]["ownerReferences"] = serde_json::json!([{"apiVersion":"v1","kind":"ConfigMap","name":"parent","uid":"parent-uid","controller":false}]);
		cluster_state.retain(|resource| resource["metadata"]["name"] != "delete-this");
		cluster_state.extend([ignored, owned, false_owner]);
		let (_server, connection) = test_utils::setup_connection_from_cluster_state(
			cluster_state,
			DiscoveryMode::Aggregated,
			false,
		)
		.await;
		let mut opts = default_prune_opts();
		opts.dry_run = Some("client".to_string());
		let diffs = run_prune(&env_dir, connection, opts)
			.await
			.expect("prune should succeed");
		assert_eq!(
			diffs
				.iter()
				.map(|diff| diff.name.as_str())
				.collect::<Vec<_>>(),
			vec!["false-owner"]
		);
	}

	#[tokio::test]
	async fn list_ignored_includes_desired_resources_without_deleting() {
		let test_dir = fixture_dir("resource_deleted");
		let env_dir = test_dir.join("environment");
		let mut cluster_state = test_utils::load_manifests_from_dir(&test_dir.join("cluster"));
		for resource in &mut cluster_state {
			resource["metadata"]["annotations"]["tanka.dev/prune-ignore"] = "true".into();
		}
		let (server, connection) = test_utils::setup_connection_from_cluster_state(
			cluster_state,
			DiscoveryMode::Aggregated,
			false,
		)
		.await;
		let mut opts = default_prune_opts();
		opts.list_ignored = true;
		let mut output = Vec::new();
		let diffs = prune_environment(
			&env_dir,
			Some(connection.clone()),
			rtk_jsonnet::Options::default(),
			opts,
			&mut output,
		)
		.await
		.expect("listing should succeed");
		assert!(diffs.is_empty());
		let output = String::from_utf8(output).unwrap();
		assert!(output.contains("ConfigMap/delete-this (namespace: default)"));
		assert!(output.contains("ConfigMap/keep-this (namespace: default)"));
		let mut filtered_opts = default_prune_opts();
		filtered_opts.list_ignored = true;
		filtered_opts.namespace = Some("default".to_string());
		filtered_opts.target = vec!["ConfigMap/keep-this".to_string()];
		let mut filtered_output = Vec::new();
		prune_environment(
			&env_dir,
			Some(connection),
			rtk_jsonnet::Options::default(),
			filtered_opts,
			&mut filtered_output,
		)
		.await
		.expect("filtered listing should succeed");
		assert_eq!(
			String::from_utf8(filtered_output).unwrap(),
			"ConfigMap/keep-this (namespace: default)\n"
		);
		assert!(server
			.http_exchanges()
			.iter()
			.all(|exchange| exchange.method != "DELETE"));
	}

	#[tokio::test]
	async fn list_ignored_does_not_evaluate_static_manifests() {
		let project = tempfile::Builder::new()
			.prefix("rustanka-prune-")
			.tempdir()
			.unwrap();
		std::fs::write(project.path().join("jsonnetfile.json"), "{}").unwrap();
		std::fs::write(
			project.path().join("main.jsonnet"),
			"error 'manifests must not be evaluated'",
		)
		.unwrap();
		std::fs::write(project.path().join("spec.json"), r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"static-prune"},"spec":{"namespace":"default"}}"#).unwrap();
		let engine = rtk_environments::Engine::new(rtk_jsonnet::Engine::new(
			rtk_jsonnet::Options::default(),
		));
		let label = engine
			.peek_single(project.path(), None)
			.unwrap()
			.unwrap()
			.environment_label();
		let test_dir = fixture_dir("resource_deleted");
		let mut cluster_state = test_utils::load_manifests_from_dir(&test_dir.join("cluster"));
		cluster_state.retain(|resource| resource["metadata"]["name"] == "delete-this");
		cluster_state[0]["metadata"]["labels"]["tanka.dev/environment"] = label.into();
		cluster_state[0]["metadata"]["annotations"]["tanka.dev/prune-ignore"] = "true".into();
		let (_server, connection) = test_utils::setup_connection_from_cluster_state(
			cluster_state,
			DiscoveryMode::Aggregated,
			false,
		)
		.await;
		let mut opts = default_prune_opts();
		opts.list_ignored = true;
		let mut output = Vec::new();
		prune_environment(
			project.path(),
			Some(connection),
			rtk_jsonnet::Options::default(),
			opts,
			&mut output,
		)
		.await
		.expect("listing should not evaluate manifests");
		assert_eq!(
			String::from_utf8(output).unwrap(),
			"ConfigMap/delete-this (namespace: default)\n"
		);
	}
}
