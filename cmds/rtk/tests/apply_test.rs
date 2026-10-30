//! Integration tests for the apply command using a mock Kubernetes API server.
//!
//! These tests call the actual `apply_environment` entrypoint with a mock
//! ClusterConnection, testing the full apply flow from Jsonnet evaluation through
//! to cluster apply.

#[path = "test_utils.rs"]
mod test_utils;

use std::path::Path;

use k8s_mock::DiscoveryMode;
use rtk::commands::{
	apply::{apply_environment, ApplyOpts, ApplyStrategy, AutoApprove},
	diff::ColorMode,
};
use rtk::k8s::diff::DiffStrategy;
use rtk_spec::canonical::EnvironmentSpec;
use serde_json::json;

/// Run an apply test.
///
/// Test structure:
/// - `environment/` - Tanka environment directory
///   - `main.jsonnet` - environment producing manifests
///   - `jsonnetfile.json` - marks the project root
/// - `cluster/` - YAML files representing initial cluster state
///
/// The test:
/// 1. Applies the environment to the mock cluster
/// 2. Verifies no error occurred
/// 3. Runs another apply and verifies no changes are detected
async fn run_apply_test(test_dir: &std::path::Path, discovery_mode: DiscoveryMode) {
	let env_dir = test_dir.join("environment");
	let cluster_dir = test_dir.join("cluster");

	// Load cluster state as manifests
	let cluster_state = test_utils::load_manifests_from_dir(&cluster_dir);
	let (_server, connection) =
		test_utils::setup_connection_from_cluster_state(cluster_state, discovery_mode, false).await;

	// Capture output to a buffer
	let mut output = Vec::new();

	// First apply: should succeed (with changes)
	let opts = ApplyOpts {
		auto_approve: AutoApprove::Always,
		color: ColorMode::Never,
		..Default::default()
	};

	let result = apply_environment(
		env_dir.as_path(),
		Some(connection.clone()),
		rtk_jsonnet::Options::default(),
		opts,
		&mut output,
	)
	.await;

	let _diffs = result.expect("first apply should succeed");
	// First apply may or may not have changes depending on initial cluster state

	// Second apply: should show no changes
	output.clear();
	let opts = ApplyOpts {
		auto_approve: AutoApprove::Always,
		color: ColorMode::Never,
		..Default::default()
	};

	let result = apply_environment(
		env_dir.as_path(),
		Some(connection),
		rtk_jsonnet::Options::default(),
		opts,
		&mut output,
	)
	.await;

	let diffs = result.expect("second apply should succeed");

	// After apply, there should be no changes
	let has_changes = diffs.iter().any(|d| d.has_changes());
	assert!(
		!has_changes,
		"expected no changes after apply, but found changes: {:?}",
		diffs
			.iter()
			.filter(|d| d.has_changes())
			.map(|d| format!("{}/{}", d.gvk.kind, d.name))
			.collect::<Vec<_>>()
	);
}

/// Run an apply that expects a Kubernetes API write failure, and assert the
/// underlying API message is preserved in the returned error chain.
async fn run_apply_failure_preserves_api_error(test_dir: &std::path::Path) {
	let env_dir = test_dir.join("environment");
	let cluster_dir = test_dir.join("cluster");
	let cluster_state = test_utils::load_manifests_from_dir(&cluster_dir);

	let api_message =
		"configmaps \"my-config\" is forbidden: User \"test\" cannot patch resource \"configmaps\"";
	let fail_writes = serde_json::json!({
		"kind": "Status",
		"apiVersion": "v1",
		"metadata": {},
		"status": "Failure",
		"message": api_message,
		"reason": "Forbidden",
		"code": 403
	});

	let server = k8s_mock::HttpMockK8sServer::builder()
		.discovery_mode(DiscoveryMode::Aggregated)
		.resources(cluster_state)
		.fail_writes(fail_writes)
		.build()
		.start()
		.await;

	let mut spec = EnvironmentSpec::default();
	spec.context_names = vec!["mock-context".into()];
	let connection =
		rtk::k8s::client::ClusterConnection::from_spec_with_kubeconfig(&spec, server.kubeconfig())
			.await
			.expect("failed to create connection");

	let mut output = Vec::new();
	let opts = ApplyOpts {
		auto_approve: AutoApprove::Always,
		color: ColorMode::Never,
		..Default::default()
	};

	let err = apply_environment(
		env_dir.as_path(),
		Some(connection),
		rtk_jsonnet::Options::default(),
		opts,
		&mut output,
	)
	.await
	.expect_err("apply should fail when the API rejects writes");

	let rendered = format!("{err:?}");
	assert!(
		rendered.contains(api_message),
		"expected kube API message in error chain, got:\n{rendered}"
	);
	assert!(
		rendered.contains("failed to apply ConfigMap/my-config"),
		"expected apply context in error, got:\n{rendered}"
	);
}

/// Run an apply test that expects no changes (idempotent case).
///
/// The environment's manifests already match the cluster state.
async fn run_apply_no_changes_test(test_dir: &std::path::Path, discovery_mode: DiscoveryMode) {
	let env_dir = test_dir.join("environment");
	let cluster_dir = test_dir.join("cluster");

	// Load cluster state as manifests
	let cluster_state = test_utils::load_manifests_from_dir(&cluster_dir);
	let (_server, connection) =
		test_utils::setup_connection_from_cluster_state(cluster_state, discovery_mode, false).await;

	// Capture output to a buffer
	let mut output = Vec::new();

	let opts = ApplyOpts {
		auto_approve: AutoApprove::Always,
		color: ColorMode::Never,
		..Default::default()
	};

	let result = apply_environment(
		env_dir.as_path(),
		Some(connection),
		rtk_jsonnet::Options::default(),
		opts,
		&mut output,
	)
	.await;

	let diffs = result.expect("apply should succeed");

	// Should have no changes since cluster matches environment
	let has_changes = diffs.iter().any(|d| d.has_changes());
	assert!(
		!has_changes,
		"expected no changes when cluster matches environment"
	);
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Generate apply tests for both aggregated and legacy discovery modes.
	macro_rules! apply_test {
		($name:ident) => {
			paste::paste! {
				#[tokio::test]
				async fn [<$name _aggregated_discovery>]() {
					let test_dir = test_utils::diff_fixture_dir(stringify!($name));
					run_apply_test(&test_dir, DiscoveryMode::Aggregated).await;
				}

				#[tokio::test]
				async fn [<$name _legacy_discovery>]() {
					let test_dir = test_utils::diff_fixture_dir(stringify!($name));
					run_apply_test(&test_dir, DiscoveryMode::Legacy).await;
				}
			}
		};
	}

	/// Generate apply tests that expect no changes.
	macro_rules! apply_no_changes_test {
		($name:ident) => {
			paste::paste! {
				#[tokio::test]
				async fn [<$name _no_changes_aggregated>]() {
					let test_dir = test_utils::diff_fixture_dir(stringify!($name));
					run_apply_no_changes_test(&test_dir, DiscoveryMode::Aggregated).await;
				}
			}
		};
	}

	// Apply tests using existing diff test fixtures
	// These test that apply works and subsequent apply shows no changes
	apply_test!(configmap_modified);
	apply_test!(multi_resource);
	apply_test!(deployment_modified);
	apply_test!(deployment_nested_changes);

	// No-changes tests: cluster already matches environment
	apply_no_changes_test!(configmap_unchanged);

	#[tokio::test]
	async fn apply_failure_preserves_kube_api_error() {
		let test_dir = test_utils::diff_fixture_dir("configmap_modified");
		run_apply_failure_preserves_api_error(&test_dir).await;
	}

	/// The CRD is registered but the object is not. Strategic merge patch returns
	/// 415 for custom resources, so the missing object only 404s on the merge
	/// patch retry, and apply must create it there.
	#[tokio::test]
	async fn apply_creates_missing_custom_resource() {
		let test_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
			.join("tests/testdata/apply")
			.join("scaledobject_create");
		let env_dir = test_dir.join("environment");
		let cluster_state = test_utils::load_manifests_from_dir(&test_dir.join("cluster"));
		let (_server, connection) = test_utils::setup_connection_from_cluster_state(
			cluster_state,
			DiscoveryMode::Aggregated,
			true,
		)
		.await;

		let mut output = Vec::new();
		let opts = ApplyOpts {
			auto_approve: AutoApprove::Always,
			color: ColorMode::Never,
			..Default::default()
		};

		apply_environment(
			env_dir.as_path(),
			Some(connection.clone()),
			rtk_jsonnet::Options::default(),
			opts,
			&mut output,
		)
		.await
		.expect("creating a missing ScaledObject should succeed");

		output.clear();
		let opts = ApplyOpts {
			auto_approve: AutoApprove::Always,
			color: ColorMode::Never,
			..Default::default()
		};
		let diffs = apply_environment(
			env_dir.as_path(),
			Some(connection),
			rtk_jsonnet::Options::default(),
			opts,
			&mut output,
		)
		.await
		.expect("second apply should succeed");

		let has_changes = diffs.iter().any(|d| d.has_changes());
		assert!(
			!has_changes,
			"expected no changes after creating the ScaledObject"
		);
	}
}

#[tokio::test(flavor = "multi_thread")]
async fn subset_array_changes_match_tanka_and_are_applied() {
	for (live_names, desired_names) in [
		(vec!["first"], vec!["first", "second"]),
		(vec!["first", "second"], vec!["first"]),
		(vec![], vec!["first"]),
		(vec!["first"], vec![]),
	] {
		let manifest = |names: &[&str]| {
			json!({
				"apiVersion": "apps/v1", "kind": "Deployment",
				"metadata": {"name": "array-repro", "namespace": "default"},
				"spec": {"replicas": 0, "selector": {"matchLabels": {"app": "array-repro"}},
					"template": {"metadata": {"labels": {"app": "array-repro"}},
						"spec": {"containers": names.iter().map(|name| json!({"name": name, "image": "busybox:1.37"})).collect::<Vec<_>>()}}}
			})
		};
		let (server, connection) = test_utils::setup_connection_from_cluster_state(
			vec![manifest(&live_names)],
			k8s_mock::DiscoveryMode::Aggregated,
			false,
		)
		.await;
		let project = tempfile::tempdir().unwrap();
		std::fs::write(project.path().join("jsonnetfile.json"), "{}").unwrap();
		std::fs::write(project.path().join("spec.json"), r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"array-repro"},"spec":{"contextNames":["mock-context"],"namespace":"default"}}"#).unwrap();
		std::fs::write(
			project.path().join("main.jsonnet"),
			format!("{{\"deployment\": {}}}", manifest(&desired_names)),
		)
		.unwrap();
		let kubeconfig = project.path().join("kubeconfig");
		std::fs::write(
			&kubeconfig,
			rtk_yaml::to_string(&server.kubeconfig()).unwrap(),
		)
		.unwrap();
		let tk = std::process::Command::new("tk")
			.args(["diff", ".", "--diff-strategy", "subset"])
			.current_dir(project.path())
			.env("KUBECONFIG", &kubeconfig)
			.output()
			.unwrap();
		assert_eq!(
			tk.status.code(),
			Some(16),
			"Tanka must detect array length changes: {}",
			String::from_utf8_lossy(&tk.stderr)
		);

		let expected_changes: &[bool] = if desired_names.len() > live_names.len() {
			&[true, false]
		} else {
			&[true]
		};
		for &changed in expected_changes {
			let diffs = apply_environment(
				project.path(),
				Some(connection.clone()),
				rtk_jsonnet::Options::default(),
				ApplyOpts {
					diff_strategy: Some(DiffStrategy::Subset),
					apply_strategy: Some(ApplyStrategy::Server),
					auto_approve: AutoApprove::Always,
					color: ColorMode::Never,
					..Default::default()
				},
				&mut Vec::new(),
			)
			.await
			.unwrap();
			assert_eq!(
				diffs.iter().any(|diff| diff.has_changes()),
				changed,
				"{live_names:?} -> {desired_names:?}"
			);
		}
		let writes: Vec<_> = server
			.http_exchanges()
			.into_iter()
			.filter(|exchange| {
				exchange.method == "PATCH"
					&& !exchange
						.query
						.as_deref()
						.unwrap_or_default()
						.contains("dryRun")
			})
			.collect();
		assert_eq!(writes.len(), 1);
		let applied: serde_json::Value = serde_json::from_str(&writes[0].request_body).unwrap();
		assert_eq!(
			applied["spec"]["template"]["spec"]["containers"],
			manifest(&desired_names)["spec"]["template"]["spec"]["containers"]
		);
	}
}
