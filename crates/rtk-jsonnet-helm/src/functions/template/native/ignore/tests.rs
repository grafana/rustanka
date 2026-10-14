use super::{Glob, Rules};

#[test]
fn helm_ordered_rules_and_defaults() {
	for (pattern, name, directory, ignored) in [
		("helm.txt", "helm.txt", false, true),
		("helm.*", "helm.txt", false, true),
		("helm.*", "rudder.txt", false, false),
		("*.txt", "tiller.txt", false, true),
		("*.txt", "cargo/a.txt", false, true),
		("cargo/*.txt", "cargo/a.txt", false, true),
		("cargo/*.txt", "cargo/nested/a.txt", false, false),
		("cargo/*.txt", "mast/a.txt", false, false),
		("ru[c-e]?er.txt", "rudder.txt", false, true),
		(".*", ".", true, false),
		(".*", "./", true, false),
		(".*", "", true, false),
		(".*", ".joonix", false, true),
		(".*", "helm.txt", false, false),
		("cargo/", "cargo", true, true),
		("cargo/", "cargo/", true, true),
		("cargo/", "mast/", true, false),
		("cargo/", "cargo", false, false),
		("!helm.txt", "helm.txt", false, false),
		("!helm.txt", "tiller.txt", false, true),
		("!*.txt", "cargo", true, true),
		("!cargo/", "mast/", true, true),
		("!cargo/", "cargo", false, true),
		("/a.txt", "a.txt", false, true),
		("/a.txt", "cargo/a.txt", false, false),
		("/cargo/a.txt", "cargo/a.txt", false, true),
		("!helm.txt\n*.txt", "helm.txt", false, true),
		("*.txt\n!helm.txt", "helm.txt", false, true),
		(
			"\u{feff}# comment\r\n \t*.txt \t\r\n",
			"helm.txt",
			false,
			true,
		),
		("", "templates/.hidden", false, true),
		("", "templates/.hidden", true, true),
		("", "templates/nested/.hidden", false, false),
		("", "crds/.hidden", false, false),
	] {
		assert_eq!(
			Rules::parse(pattern).unwrap().ignores(name, directory),
			ignored,
			"pattern {pattern:?}, path {name:?}, directory {directory}"
		);
	}
}

#[test]
fn go_globs_match_unicode_classes_and_escaped_metacharacters() {
	for (pattern, name, matched) in [
		("*.yaml", "a\nb.yaml", true),
		("?.yaml", "é.yaml", true),
		("?.yaml", "ab.yaml", false),
		("[a-c].yaml", "b.yaml", true),
		("[^a-c].yaml", "é.yaml", true),
		("[^a-c].yaml", "a.yaml", false),
		("[!a].yaml", "!.yaml", true),
		("[!a].yaml", "b.yaml", false),
		("[z-a].yaml", "z.yaml", false),
		(r"\*.yaml", "*.yaml", true),
		(r"\?.yaml", "?.yaml", true),
		(r"\[a\].yaml", "[a].yaml", true),
		(r"[\-].yaml", "-.yaml", true),
		(r"[\]].yaml", "].yaml", true),
		(r"back\\slash.yaml", r"back\slash.yaml", true),
		("a*b*c", "a123b456c", true),
		("a*b*c", "a/bc", false),
		("[/]", "/", true),
		("?", "/", false),
	] {
		assert_eq!(
			Glob::parse(pattern).unwrap().matches(name),
			matched,
			"pattern {pattern:?}, path {name:?}"
		);
	}
}

#[test]
fn malformed_helmignore_rules_are_rejected() {
	for pattern in [
		"foo/**/bar",
		"**",
		"[z-",
		"[",
		"[]",
		"[a-]",
		"[a",
		"[a\\]",
		"\\",
	] {
		assert!(Rules::parse(pattern).is_err(), "pattern {pattern:?}");
	}
	assert!(Rules::parse(&"x".repeat(65_536)).is_err());
}
