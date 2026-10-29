use k8s_mock::{DiscoveryMode, HttpMockK8sServer, MockApiResource, MockDiscovery};
use rtk::k8s::{
	client::ClusterConnection,
	diff::{DiffEngine, DiffStatus, DiffStrategy},
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn prune_preserves_live_uids_and_deduplicates_versions_like_tanka() {
	let project = tempfile::tempdir().unwrap();
	std::fs::write(project.path().join("jsonnetfile.json"), "{}").unwrap();
	std::fs::write(project.path().join("spec.json"), r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"versions"},"spec":{"contextNames":["mock-context"],"namespace":"default","injectLabels":true}}"#).unwrap();
	let mut desired = json!({"apiVersion":"routes.example.com/v1alpha2", "kind":"TCPRoute", "metadata":{"name":"keep", "namespace":"default"}});
	std::fs::write(
		project.path().join("main.jsonnet"),
		json!({"route": desired}).to_string(),
	)
	.unwrap();
	let show = std::process::Command::new("tk")
		.args(["show", ".", "--dangerous-allow-redirect"])
		.current_dir(project.path())
		.output()
		.unwrap();
	assert!(
		show.status.success(),
		"{}",
		String::from_utf8_lossy(&show.stderr)
	);
	let rendered: serde_json::Value = serde_saphyr::from_slice(&show.stdout).unwrap();
	let label = rendered["metadata"]["labels"]["tanka.dev/environment"]
		.as_str()
		.unwrap();
	desired["metadata"]["labels"] = json!({"tanka.dev/environment": label});

	for mode in [DiscoveryMode::Aggregated, DiscoveryMode::Legacy] {
		for orphan in [false, true] {
			let mut resources = Vec::new();
			for version in ["v1alpha2", "v1"] {
				let mut live = desired.clone();
				live["apiVersion"] = json!(format!("routes.example.com/{version}"));
				live["metadata"]["uid"] = json!("desired-uid");
				live["metadata"]["annotations"] =
					json!({"kubectl.kubernetes.io/last-applied-configuration":"{}"});
				resources.push(live.clone());
				if orphan {
					live["metadata"]["name"] = json!("orphan");
					live["metadata"]["uid"] = json!("orphan-uid");
					resources.push(live);
				}
			}
			let discovery = MockDiscovery::default()
				.with_group(
					"routes.example.com/v1alpha2",
					vec![MockApiResource::namespaced("tcproutes", "TCPRoute")],
				)
				.with_group(
					"routes.example.com/v1",
					vec![MockApiResource::namespaced("tcproutes", "TCPRoute")],
				);
			let server = HttpMockK8sServer::builder()
				.discovery_mode(mode)
				.discovery(discovery)
				.resources(resources)
				.build()
				.start()
				.await;
			let config = project.path().join("kubeconfig");
			std::fs::write(&config, rtk_yaml::to_string(&server.kubeconfig()).unwrap()).unwrap();
			let tk = std::process::Command::new("tk")
				.args(["diff", ".", "--with-prune", "--diff-strategy", "subset"])
				.current_dir(project.path())
				.env("KUBECONFIG", &config)
				.output()
				.unwrap();
			assert_eq!(
				tk.status.code(),
				Some(if orphan { 16 } else { 0 }),
				"{}",
				String::from_utf8_lossy(&tk.stderr)
			);
			let stdout = String::from_utf8_lossy(&tk.stdout);
			assert!(
				!stdout.contains("name: keep"),
				"desired object must not be deleted: {stdout}"
			);
			if orphan {
				assert_eq!(stdout.matches("name: orphan").count(), 1, "{stdout}");
			}
			let mut spec = rtk_spec::canonical::EnvironmentSpec::default();
			spec.context_names = vec!["mock-context".into()];
			let connection =
				ClusterConnection::from_spec_with_kubeconfig(&spec, server.kubeconfig())
					.await
					.unwrap();
			let manifests = vec![desired.clone()];
			let engine = DiffEngine::new(
				connection,
				DiffStrategy::Subset,
				"default".into(),
				&manifests,
				true,
			)
			.await
			.unwrap();
			let diffs = engine
				.diff_all(&manifests, true, Some(label), true)
				.await
				.unwrap();
			let deleted: Vec<_> = diffs
				.iter()
				.filter(|diff| diff.status == DiffStatus::Deleted)
				.collect();
			assert_eq!(deleted.len(), usize::from(orphan));
			if orphan {
				assert_eq!(deleted[0].name, "orphan");
			}
		}
	}
}
