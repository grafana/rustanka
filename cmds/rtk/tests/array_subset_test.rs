#[path = "test_utils.rs"]
mod test_utils;

use rtk::commands::{
	apply::{apply_environment, ApplyOpts, ApplyStrategy, AutoApprove},
	diff::ColorMode,
};
use rtk::k8s::diff::DiffStrategy;
use serde_json::json;

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
