use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::{Function, Options};
use crate::State;
use crate::cache::Key;

fn fixture() -> (TempDir, Function, Options) {
	let directory = tempdir().unwrap();
	fs::write(
		directory.path().join("Chart.yaml"),
		"apiVersion: v2\nname: example\nversion: 1.0.0\n",
	)
	.unwrap();
	fs::create_dir(directory.path().join("templates")).unwrap();
	fs::write(directory.path().join("templates/config.yaml"), "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: example\ndata:\n  value: {{ .Values.value | default \"original\" | quote }}\n").unwrap();
	let options = serde_json::from_value(
		json!({"calledFrom": directory.path().join("main.jsonnet"), "namespace": "testing"}),
	)
	.unwrap();
	let mut state = State::new(Some(|_: &Path| -> Option<PathBuf> {
		panic!("native rendering must not resolve a disk cache directory")
	}));
	state.helm_binary = directory.path().join("nonexistent-helm");
	(directory, Function::new(Arc::new(state)), options)
}

#[test]
fn native_parallel_calls_render_once_across_functions_sharing_state() {
	let (directory, function, options) = fixture();
	let renders = AtomicUsize::new(0);
	let start = Barrier::new(8);
	std::thread::scope(|scope| {
		let handles = (0..8)
			.map(|_| {
				let function = Function::new(Arc::clone(&function.state));
				let (chart, options, renders, start) =
					(directory.path(), &options, &renders, &start);
				scope.spawn(move || {
					start.wait();
					function
						.cached_native_or_compute("release", chart, options, || {
							renders.fetch_add(1, Ordering::SeqCst);
							super::native::Chart::load(chart)?.render("release", options)
						})
						.unwrap()
				})
			})
			.collect::<Vec<_>>();
		for handle in handles {
			assert_eq!(
				handle.join().unwrap()["config_map_example"]["data"]["value"],
				"original"
			);
		}
	});
	assert_eq!(renders.load(Ordering::SeqCst), 1);
	assert!(function.state.helm_identity.get().is_none());
	assert!(function.state.helm_namespace.get().is_none());
	assert!(!directory.path().join("target").exists());
}

#[test]
fn native_cache_does_not_hide_chart_or_option_changes() {
	let (directory, function, mut options) = fixture();
	let render = |options: &Options| {
		function
			.native_render("release", directory.path(), options, false)
			.unwrap()
	};
	assert_eq!(
		render(&options)["config_map_example"]["data"]["value"],
		"original"
	);
	options.values = Some(json!({"value": "override"}));
	assert_eq!(
		render(&options)["config_map_example"]["data"]["value"],
		"override"
	);
	options.values = None;
	fs::write(directory.path().join("values.yaml"), "value: changed\n").unwrap();
	assert_eq!(
		render(&options)["config_map_example"]["data"]["value"],
		"changed"
	);
	fs::create_dir_all(directory.path().join("charts/empty-dependency")).unwrap();
	assert!(
		function
			.native_render("release", directory.path(), &options, false)
			.unwrap_err()
			.to_string()
			.contains("subcharts")
	);
}

#[test]
fn native_failed_or_changing_renders_are_not_cached() {
	let (directory, function, options) = fixture();
	let chart = directory.path();
	assert_eq!(
		function.cached_native_or_compute(
			"release",
			chart,
			&options,
			|| Err::<serde_json::Value, _>("failure")
		),
		Err("failure")
	);
	function
		.cached_native_or_compute("release", chart, &options, || {
			fs::write(chart.join("values.yaml"), "value: changed\n").unwrap();
			Ok::<_, ()>(json!("stale"))
		})
		.unwrap();
	// Reverting the input must not uncover an answer computed while it changed.
	fs::remove_file(chart.join("values.yaml")).unwrap();
	let actual = function
		.native_render("release", chart, &options, false)
		.unwrap();
	assert_eq!(actual["config_map_example"]["data"]["value"], "original");
}

#[test]
fn native_bypass_ignores_cached_values_and_does_not_fill_cache() {
	let (directory, function, options) = fixture();
	let chart = directory.path();
	let key = Key::native_render("release", chart, &options).unwrap();
	let actual = function
		.native_render("release", chart, &options, true)
		.unwrap();
	assert!(function.state.cache.get(key).is_none());
	function.state.cache.insert(key, json!("cached"));
	assert_eq!(
		function
			.native_render("release", chart, &options, true)
			.unwrap(),
		actual
	);
	assert_eq!(
		function
			.native_render("release", chart, &options, false)
			.unwrap(),
		json!("cached")
	);
}

#[test]
fn native_key_separates_renderers_and_all_options() {
	let (directory, _, options) = fixture();
	let chart = directory.path();
	let key = Key::native_render("release", chart, &options).unwrap();
	assert_ne!(
		key,
		Key::render("release", chart, &options, None, None).unwrap()
	);
	assert_ne!(key, Key::native_render("other", chart, &options).unwrap());
	for (field, value) in [
		("namespace", json!("other")),
		("namespace", json!(null)),
		("apiVersions", json!(["example.test/v1"])),
		("nameFormat", json!("{{ .metadata.namespace }}")),
		("noHooks", json!(true)),
		("includeCrds", json!(false)),
		("values", json!({"value": "other"})),
	] {
		let mut changed = json!({"calledFrom": "elsewhere/main.jsonnet", "namespace": "testing"});
		changed[field] = value;
		let changed = serde_json::from_value(changed).unwrap();
		assert_ne!(
			key,
			Key::native_render("release", chart, &changed).unwrap(),
			"{field}"
		);
	}
}
