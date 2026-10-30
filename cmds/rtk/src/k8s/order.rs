use std::cmp::Ordering;

pub(crate) fn compare_manifests(left: &serde_json::Value, right: &serde_json::Value) -> Ordering {
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

pub(crate) fn kind_position(kind: &str) -> usize {
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
