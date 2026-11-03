use rtk::{
	commands::{
		apply::{apply_environment, ApplyOpts, ApplyStrategy, AutoApprove},
		diff::ColorMode,
	},
	k8s::{client::ClusterConnection, diff::DiffStrategy},
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn applies_in_tanka_dependency_order() {
	let _ = rustls::crypto::ring::default_provider().install_default();
	let namespace =
		json!({"apiVersion":"v1", "kind":"Namespace", "metadata":{"name":"ordering-repro"}});
	let config = json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"config", "namespace":"ordering-repro"}, "data":{"key":"value"}});
	let crd = json!({"apiVersion":"apiextensions.k8s.io/v1", "kind":"CustomResourceDefinition", "metadata":{"name":"widgets.test.example.com"}, "spec":{"group":"test.example.com", "scope":"Namespaced", "names":{"kind":"Widget", "plural":"widgets", "singular":"widget"}, "versions":[{"name":"v1", "served":true, "storage":true, "schema":{"openAPIV3Schema":{"type":"object"}}}]}});
	let widget = json!({"apiVersion":"test.example.com/v1", "kind":"Widget", "metadata":{"name":"example", "namespace":"ordering-repro"}});
	let project = tempfile::tempdir().unwrap();
	std::fs::write(project.path().join("jsonnetfile.json"), "{}").unwrap();
	std::fs::write(project.path().join("spec.json"), r#"{"apiVersion":"tanka.dev/v1alpha1","kind":"Environment","metadata":{"name":"ordering"},"spec":{"contextNames":["mock-context"],"namespace":"ordering-repro"}}"#).unwrap();
	std::fs::write(
		project.path().join("main.jsonnet"),
		json!({"a":widget,"b":config,"c":crd,"d":namespace}).to_string(),
	)
	.unwrap();
	let tk = std::process::Command::new("tk")
		.args(["show", ".", "--dangerous-allow-redirect"])
		.current_dir(project.path())
		.output()
		.unwrap();
	assert!(
		tk.status.success(),
		"{}",
		String::from_utf8_lossy(&tk.stderr)
	);
	let yaml = String::from_utf8(tk.stdout).unwrap();
	let expected: Vec<_> = yaml
		.lines()
		.filter_map(|line| line.strip_prefix("kind: "))
		.collect();
	assert_eq!(
		expected,
		[
			"Namespace",
			"ConfigMap",
			"CustomResourceDefinition",
			"Widget"
		]
	);

	for discovery_mode in [
		k8s_mock::DiscoveryMode::Aggregated,
		k8s_mock::DiscoveryMode::Legacy,
	] {
		for strategy in [ApplyStrategy::Server, ApplyStrategy::Client] {
			for diff_strategy in [
				DiffStrategy::Native,
				DiffStrategy::Server,
				DiffStrategy::Subset,
				DiffStrategy::Validate,
			] {
				let (discovery, schemas) = k8s_mock::extract_crd_metadata(&[crd.clone()]);
				let discovery = discovery.with_group(
					"apiextensions.k8s.io/v1",
					vec![k8s_mock::MockApiResource::cluster_scoped(
						"customresourcedefinitions",
						"CustomResourceDefinition",
					)],
				);
				let server = k8s_mock::HttpMockK8sServer::builder()
					.discovery_mode(discovery_mode)
					.discovery(discovery)
					.openapi_schemas(schemas)
					.build()
					.start()
					.await;
				server
					.defer_group_until_crd("test.example.com", "widgets.test.example.com")
					.await;
				let rejected = reqwest::Client::new()
					.post(format!(
						"{}/apis/test.example.com/v1/namespaces/ordering-repro/widgets",
						server.uri()
					))
					.body(widget.to_string())
					.send()
					.await
					.unwrap();
				assert_eq!(rejected.status().as_u16(), 404);
				let mut spec = rtk_spec::canonical::EnvironmentSpec::default();
				spec.context_names = vec!["mock-context".into()];
				let connection =
					ClusterConnection::from_spec_with_kubeconfig(&spec, server.kubeconfig())
						.await
						.unwrap();
				apply_environment(
					project.path(),
					Some(connection),
					rtk_jsonnet::Options::default(),
					ApplyOpts {
						diff_strategy: Some(diff_strategy),
						apply_strategy: Some(strategy),
						auto_approve: AutoApprove::Always,
						color: ColorMode::Never,
						..Default::default()
					},
					&mut Vec::new(),
				)
				.await
				.unwrap();
				let kinds: Vec<String> = server
					.http_exchanges()
					.into_iter()
					.filter(|exchange| {
						["POST", "PATCH"].contains(&exchange.method.as_str())
							&& (200..300).contains(&exchange.response_status)
							&& !exchange
								.query
								.as_deref()
								.unwrap_or_default()
								.contains("dryRun")
					})
					.map(|exchange| {
						serde_json::from_str::<serde_json::Value>(&exchange.request_body).unwrap()
							["kind"]
							.as_str()
							.unwrap()
							.to_owned()
					})
					.collect();
				assert_eq!(kinds, expected, "{strategy:?}");
			}
		}
	}
}

#[tokio::test]
async fn pending_crds_are_added_without_a_namespace_pending_message() {
	use rtk::k8s::diff::{DiffEngine, DiffStatus};
	for scope in ["Namespaced", "Cluster"] {
		for strategy in [
			DiffStrategy::Native,
			DiffStrategy::Server,
			DiffStrategy::Subset,
			DiffStrategy::Validate,
		] {
			let crd = json!({"apiVersion":"apiextensions.k8s.io/v1", "kind":"CustomResourceDefinition", "metadata":{"name":"widgets.test.example.com"}, "spec":{"group":"test.example.com", "scope":scope, "names":{"kind":"Widget", "plural":"widgets"}, "versions":[{"name":"v1", "served":true, "storage":true}]}});
			let widget = json!({"apiVersion":"test.example.com/v1", "kind":"Widget", "metadata":{"name":"example", "namespace":"default"}});
			let discovery = k8s_mock::MockDiscovery::default().with_group(
				"apiextensions.k8s.io/v1",
				vec![k8s_mock::MockApiResource::cluster_scoped(
					"customresourcedefinitions",
					"CustomResourceDefinition",
				)],
			);
			let server = k8s_mock::HttpMockK8sServer::builder()
				.discovery(discovery)
				.resources(vec![
					json!({"apiVersion":"v1", "kind":"Namespace", "metadata":{"name":"default"}}),
				])
				.build()
				.start()
				.await;
			let mut spec = rtk_spec::canonical::EnvironmentSpec::default();
			spec.context_names = vec!["mock-context".into()];
			let connection =
				ClusterConnection::from_spec_with_kubeconfig(&spec, server.kubeconfig())
					.await
					.unwrap();
			let manifests = vec![crd, widget];
			let engine = DiffEngine::new(connection, strategy, "default".into(), &manifests, false)
				.await
				.unwrap();
			let diffs = engine
				.diff_all(&manifests, false, None, false)
				.await
				.unwrap();
			let diff = diffs.iter().find(|diff| diff.gvk.kind == "Widget").unwrap();
			assert_eq!(diff.status, DiffStatus::Added, "{scope}, {strategy:?}");
			assert!(diff.current_yaml.is_empty());
			assert!(diff.desired_yaml.contains("kind: Widget"));
			let mut output = Vec::new();
			let mut writer =
				rtk::k8s::output::DiffOutput::new(&mut output, ColorMode::Never, strategy).unwrap();
			writer.write_diff(diff).unwrap();
			writer.write_summary(std::slice::from_ref(diff)).unwrap();
			let output = String::from_utf8(output).unwrap();
			assert!(!output.contains("namespace not yet created"), "{output}");
			assert!(!output.contains("namespace pending"), "{output}");
		}
	}
}
