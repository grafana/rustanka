use sha2::Digest as _;

fn main() {
	println!("cargo:rustc-check-cfg=cfg(nightly)");
	if rustversion::cfg!(nightly) {
		println!("cargo:rustc-cfg=nightly");
	}
	let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
		.parent()
		.expect("crates directory")
		.parent()
		.expect("workspace directory");
	let mut hasher = sha2::Sha256::new();
	for crate_name in ["jrsonnet-evaluator", "jrsonnet-ir", "jrsonnet-interner"] {
		let dir = root.join("crates").join(crate_name);
		hash_dir(&dir, &mut hasher);
	}
	let lock = root.join("Cargo.lock");
	println!("cargo:rerun-if-changed={}", lock.display());
	sha2::Digest::update(&mut hasher, std::fs::read(lock).expect("Cargo.lock"));
	for (name, value) in std::env::vars() {
		if name.starts_with("CARGO_FEATURE_")
			|| name.starts_with("CARGO_CFG_TARGET_")
			|| name == "RUSTFLAGS"
		{
			sha2::Digest::update(&mut hasher, name.as_bytes());
			sha2::Digest::update(&mut hasher, value.as_bytes());
		}
	}
	let digest = sha2::Digest::finalize(hasher);
	let id: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
	println!("cargo:rustc-env=JRSONNET_PREPARED_CACHE_BUILD_ID={id}");
}

fn hash_dir(dir: &std::path::Path, hasher: &mut sha2::Sha256) {
	let mut entries: Vec<_> = std::fs::read_dir(dir)
		.expect("source directory")
		.map(|entry| entry.expect("source entry").path())
		.collect();
	entries.sort();
	for path in entries {
		if path.is_dir() {
			hash_dir(&path, hasher);
		} else if matches!(
			path.extension().and_then(|ext| ext.to_str()),
			Some("rs" | "toml")
		) {
			println!("cargo:rerun-if-changed={}", path.display());
			sha2::Digest::update(hasher, path.to_string_lossy().as_bytes());
			sha2::Digest::update(hasher, std::fs::read(path).expect("source file"));
		}
	}
}
