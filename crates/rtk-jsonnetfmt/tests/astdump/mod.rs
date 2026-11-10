//! The Rust half of the AST oracles, shared by `node_parity` and
//! `pass_parity`.
//!
//! Two oracles are generated from go-jsonnet in one notation — the parser's
//! tree (`make update-fmt-node-oracle`) and the tree each pass leaves behind
//! (`make update-fmt-pass-oracle`) — because both come from
//! `testdata/generate/nodedump/dump.go`. So this side is one dumper too: the
//! paths, the slot names in their order, and the attribute spellings all have
//! to match that file exactly. Where the two disagree about *shape* rather
//! than content, a parity test fails and the fix is usually here.
//!
//! Not every item is used by both test binaries, hence the blanket allow: a
//! `tests/` submodule is compiled separately into each.

#![allow(dead_code)]

use std::{collections::BTreeMap, fmt::Write as _, path::PathBuf};

use rtk_jsonnetfmt::{
	ast::{CommaSeparatedExpr, ForSpec, FunctionSugar, Node, NodeKind, ObjectField, Parameter},
	fodder::Fodder,
};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// The oracles' shape.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpEntry {
	pub path: String,
	pub kind: String,
	#[serde(default)]
	pub attrs: BTreeMap<String, String>,
	#[serde(default)]
	pub slots: Vec<DumpSlot>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpSlot {
	pub name: String,
	#[serde(default)]
	pub fodder: Vec<DumpFodderElement>,
}

#[derive(Debug, Deserialize)]
pub struct DumpFodderElement {
	pub kind: String,
	pub blanks: usize,
	pub indent: usize,
	#[serde(default)]
	pub comment: Vec<String>,
}

/// One entry of a snippets file. `why` documents the case for human readers.
#[derive(Debug, Deserialize)]
pub struct Snippet {
	pub name: String,
	pub source: String,
	pub why: String,
}

/// Render a dumped element in the notation [`FodderElement::describe`]
/// produces.
///
/// Both halves are rendered on this side so that no difference between Go's
/// string escaping and Rust's can be mistaken for a difference in fodder.
///
/// [`FodderElement::describe`]: rtk_jsonnetfmt::fodder::FodderElement::describe
pub fn describe_dumped(fodder: &[DumpFodderElement]) -> Vec<String> {
	fodder
		.iter()
		.map(|element| {
			format!(
				"{}(blanks={}, indent={}, comment={:?})",
				element.kind, element.blanks, element.indent, element.comment
			)
		})
		.collect()
}

// ---------------------------------------------------------------------------
// The port's side of the same dump.
// ---------------------------------------------------------------------------

/// One node or fodder-carrying helper struct, in the oracles' shape.
///
/// Comparable so that `pass_parity` can check a pass left a tree *alone*,
/// which is what most of the oracle's cells claim.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
	pub path: String,
	pub kind: String,
	pub attrs: BTreeMap<String, String>,
	pub slots: Vec<(String, Vec<String>)>,
}

/// The kind recorded where something optional is absent.
pub const NIL: &str = "<nil>";

#[derive(Default)]
pub struct Dumper {
	entries: Vec<Entry>,
}

fn attrs<const N: usize>(pairs: [(&str, String); N]) -> BTreeMap<String, String> {
	pairs
		.into_iter()
		.map(|(key, value)| (key.to_owned(), value))
		.collect()
}

/// A node that carries nothing but fodder. The Go dumper passes a nil map,
/// which `omitempty` drops, and serde defaults the missing field to this.
fn no_attrs() -> BTreeMap<String, String> {
	BTreeMap::new()
}

fn slot(name: &str, fodder: &Fodder) -> (String, Vec<String>) {
	(name.to_owned(), fodder.describe())
}

fn index(path: &str, at: usize) -> String {
	format!("{path}[{at}]")
}

fn ident(id: Option<&String>) -> String {
	id.cloned().unwrap_or_else(|| NIL.to_owned())
}

impl Dumper {
	fn emit(
		&mut self,
		path: &str,
		kind: &str,
		attrs: BTreeMap<String, String>,
		slots: Vec<(String, Vec<String>)>,
	) {
		self.entries.push(Entry {
			path: path.to_owned(),
			kind: kind.to_owned(),
			attrs,
			slots,
		});
	}

	fn child(&mut self, path: &str, node: Option<&Node>) {
		match node {
			None => self.emit(path, NIL, BTreeMap::new(), Vec::new()),
			Some(node) => self.node(path, node),
		}
	}

	fn params(&mut self, path: &str, params: &[Parameter]) {
		for (at, param) in params.iter().enumerate() {
			let at = index(path, at);
			self.emit(
				&at,
				"Parameter",
				attrs([("name", param.name.clone())]),
				vec![
					slot("NameFodder", &param.name_fodder),
					slot("EqFodder", &param.eq_fodder),
					slot("CommaFodder", &param.comma_fodder),
				],
			);
			self.child(&format!("{at}.DefaultArg"), param.default_arg.as_deref());
		}
	}

	fn method(&mut self, path: &str, fun: Option<&FunctionSugar>) {
		let Some(fun) = fun else {
			self.emit(path, NIL, BTreeMap::new(), Vec::new());
			return;
		};
		self.emit(
			path,
			"Method",
			attrs([("trailingComma", fun.trailing_comma.to_string())]),
			vec![
				slot("ParenLeftFodder", &fun.paren_left_fodder),
				slot("ParenRightFodder", &fun.paren_right_fodder),
			],
		);
		self.params(&format!("{path}.Parameters"), &fun.parameters);
	}

	fn for_spec(&mut self, path: &str, spec: &ForSpec) {
		if let Some(outer) = &spec.outer {
			self.for_spec(&format!("{path}.Outer"), outer);
		}
		self.emit(
			path,
			"ForSpec",
			attrs([("varName", spec.var_name.clone())]),
			vec![
				slot("ForFodder", &spec.for_fodder),
				slot("VarFodder", &spec.var_fodder),
				slot("InFodder", &spec.in_fodder),
			],
		);
		self.node(&format!("{path}.Expr"), &spec.expr);
		for (at, condition) in spec.conditions.iter().enumerate() {
			let at = index(&format!("{path}.Conditions"), at);
			self.emit(
				&at,
				"IfSpec",
				BTreeMap::new(),
				vec![slot("IfFodder", &condition.if_fodder)],
			);
			self.node(&format!("{at}.Expr"), &condition.expr);
		}
	}

	fn fields(&mut self, path: &str, fields: &[ObjectField]) {
		for (at, field) in fields.iter().enumerate() {
			let at = index(path, at);
			self.emit(
				&at,
				"ObjectField",
				attrs([
					("fieldKind", field.kind.name().to_owned()),
					("hide", field.hide.name().to_owned()),
					("superSugar", field.super_sugar.to_string()),
					("id", ident(field.id.as_ref())),
				]),
				vec![
					slot("Fodder1", &field.fodder1),
					slot("Fodder2", &field.fodder2),
					slot("OpFodder", &field.op_fodder),
					slot("CommaFodder", &field.comma_fodder),
				],
			);
			self.method(&format!("{at}.Method"), field.method.as_ref());
			self.child(&format!("{at}.Expr1"), field.expr1.as_deref());
			self.child(&format!("{at}.Expr2"), field.expr2.as_deref());
			self.child(&format!("{at}.Expr3"), field.expr3.as_deref());
		}
	}

	fn comma_separated(&mut self, path: &str, elements: &[CommaSeparatedExpr]) {
		for (at, element) in elements.iter().enumerate() {
			let at = index(path, at);
			self.emit(
				&at,
				"CommaSeparatedExpr",
				BTreeMap::new(),
				vec![slot("CommaFodder", &element.comma_fodder)],
			);
			// Go reaches this through `child`, whose nil branch the parser
			// cannot produce here; the entry it writes is the same.
			self.node(&format!("{at}.Expr"), &element.expr);
		}
	}

	#[allow(clippy::too_many_lines)]
	fn node(&mut self, path: &str, node: &Node) {
		let own = slot("Fodder", &node.fodder);

		match &node.kind {
			NodeKind::Apply(apply) => {
				self.emit(
					path,
					"Apply",
					attrs([
						("trailingComma", apply.trailing_comma.to_string()),
						("tailStrict", apply.tail_strict.to_string()),
					]),
					vec![
						own,
						slot("FodderLeft", &apply.fodder_left),
						slot("FodderRight", &apply.fodder_right),
						slot("TailStrictFodder", &apply.tail_strict_fodder),
					],
				);
				self.node(&format!("{path}.Target"), &apply.target);
				self.comma_separated(
					&format!("{path}.Arguments.Positional"),
					&apply.arguments.positional,
				);
				for (at, arg) in apply.arguments.named.iter().enumerate() {
					let at = index(&format!("{path}.Arguments.Named"), at);
					self.emit(
						&at,
						"NamedArgument",
						attrs([("name", arg.name.clone())]),
						vec![
							slot("NameFodder", &arg.name_fodder),
							slot("EqFodder", &arg.eq_fodder),
							slot("CommaFodder", &arg.comma_fodder),
						],
					);
					self.node(&format!("{at}.Arg"), &arg.arg);
				}
			}

			NodeKind::ApplyBrace(brace) => {
				self.emit(path, "ApplyBrace", no_attrs(), vec![own]);
				self.node(&format!("{path}.Left"), &brace.left);
				self.node(&format!("{path}.Right"), &brace.right);
			}

			NodeKind::Array(array) => {
				self.emit(
					path,
					"Array",
					attrs([("trailingComma", array.trailing_comma.to_string())]),
					vec![own, slot("CloseFodder", &array.close_fodder)],
				);
				self.comma_separated(&format!("{path}.Elements"), &array.elements);
			}

			NodeKind::ArrayComp(comp) => {
				self.emit(
					path,
					"ArrayComp",
					attrs([("trailingComma", comp.trailing_comma.to_string())]),
					vec![
						own,
						slot("TrailingCommaFodder", &comp.trailing_comma_fodder),
						slot("CloseFodder", &comp.close_fodder),
					],
				);
				self.node(&format!("{path}.Body"), &comp.body);
				self.for_spec(&format!("{path}.Spec"), &comp.spec);
			}

			NodeKind::Assert(assert) => {
				self.emit(
					path,
					"Assert",
					no_attrs(),
					vec![
						own,
						slot("ColonFodder", &assert.colon_fodder),
						slot("SemicolonFodder", &assert.semicolon_fodder),
					],
				);
				self.node(&format!("{path}.Cond"), &assert.cond);
				self.child(&format!("{path}.Message"), assert.message.as_deref());
				self.node(&format!("{path}.Rest"), &assert.rest);
			}

			NodeKind::Binary(binary) => {
				self.emit(
					path,
					"Binary",
					attrs([("op", binary.op.as_str().to_owned())]),
					vec![own, slot("OpFodder", &binary.op_fodder)],
				);
				self.node(&format!("{path}.Left"), &binary.left);
				self.node(&format!("{path}.Right"), &binary.right);
			}

			NodeKind::Conditional(conditional) => {
				self.emit(
					path,
					"Conditional",
					no_attrs(),
					vec![
						own,
						slot("ThenFodder", &conditional.then_fodder),
						slot("ElseFodder", &conditional.else_fodder),
					],
				);
				self.node(&format!("{path}.Cond"), &conditional.cond);
				self.node(&format!("{path}.BranchTrue"), &conditional.branch_true);
				self.child(
					&format!("{path}.BranchFalse"),
					conditional.branch_false.as_deref(),
				);
			}

			NodeKind::Dollar => self.emit(path, "Dollar", no_attrs(), vec![own]),

			NodeKind::Error(error) => {
				self.emit(path, "Error", no_attrs(), vec![own]);
				self.node(&format!("{path}.Expr"), &error.expr);
			}

			NodeKind::Function(function) => {
				self.emit(
					path,
					"Function",
					attrs([("trailingComma", function.trailing_comma.to_string())]),
					vec![
						own,
						slot("ParenLeftFodder", &function.paren_left_fodder),
						slot("ParenRightFodder", &function.paren_right_fodder),
					],
				);
				self.params(&format!("{path}.Parameters"), &function.parameters);
				self.node(&format!("{path}.Body"), &function.body);
			}

			NodeKind::Import(import) => {
				self.emit(path, "Import", no_attrs(), vec![own]);
				self.node(&format!("{path}.File"), &import.file);
			}
			NodeKind::ImportStr(import) => {
				self.emit(path, "ImportStr", no_attrs(), vec![own]);
				self.node(&format!("{path}.File"), &import.file);
			}
			NodeKind::ImportBin(import) => {
				self.emit(path, "ImportBin", no_attrs(), vec![own]);
				self.node(&format!("{path}.File"), &import.file);
			}

			NodeKind::Index(node_index) => {
				self.emit(
					path,
					"Index",
					attrs([("id", ident(node_index.id.as_ref()))]),
					vec![
						own,
						slot("LeftBracketFodder", &node_index.left_bracket_fodder),
						slot("RightBracketFodder", &node_index.right_bracket_fodder),
					],
				);
				self.node(&format!("{path}.Target"), &node_index.target);
				self.child(&format!("{path}.Index"), node_index.index.as_deref());
			}

			NodeKind::Slice(slice) => {
				self.emit(
					path,
					"Slice",
					no_attrs(),
					vec![
						own,
						slot("LeftBracketFodder", &slice.left_bracket_fodder),
						slot("EndColonFodder", &slice.end_colon_fodder),
						slot("StepColonFodder", &slice.step_colon_fodder),
						slot("RightBracketFodder", &slice.right_bracket_fodder),
					],
				);
				self.node(&format!("{path}.Target"), &slice.target);
				self.child(&format!("{path}.BeginIndex"), slice.begin_index.as_deref());
				self.child(&format!("{path}.EndIndex"), slice.end_index.as_deref());
				self.child(&format!("{path}.Step"), slice.step.as_deref());
			}

			NodeKind::InSuper(in_super) => {
				self.emit(
					path,
					"InSuper",
					no_attrs(),
					vec![
						own,
						slot("InFodder", &in_super.in_fodder),
						slot("SuperFodder", &in_super.super_fodder),
					],
				);
				self.node(&format!("{path}.Index"), &in_super.index);
			}

			NodeKind::Local(local) => {
				self.emit(path, "Local", no_attrs(), vec![own]);
				for (at, bind) in local.binds.iter().enumerate() {
					let at = index(&format!("{path}.Binds"), at);
					self.emit(
						&at,
						"LocalBind",
						attrs([("variable", bind.variable.clone())]),
						vec![
							slot("VarFodder", &bind.var_fodder),
							slot("EqFodder", &bind.eq_fodder),
							slot("CloseFodder", &bind.close_fodder),
						],
					);
					self.method(&format!("{at}.Fun"), bind.fun.as_ref());
					self.node(&format!("{at}.Body"), &bind.body);
				}
				self.node(&format!("{path}.Body"), &local.body);
			}

			NodeKind::LiteralBoolean(value) => self.emit(
				path,
				"LiteralBoolean",
				attrs([("value", value.to_string())]),
				vec![own],
			),

			NodeKind::LiteralNull => self.emit(path, "LiteralNull", no_attrs(), vec![own]),

			NodeKind::LiteralNumber(number) => self.emit(
				path,
				"LiteralNumber",
				attrs([("originalString", number.original_string.clone())]),
				vec![own],
			),

			NodeKind::LiteralString(literal) => self.emit(
				path,
				"LiteralString",
				attrs([
					("kind", literal.kind.name().to_owned()),
					("value", literal.value.clone()),
					("blockIndent", literal.block_indent.clone()),
					("blockTermIndent", literal.block_term_indent.clone()),
				]),
				vec![own],
			),

			NodeKind::Object(object) => {
				self.emit(
					path,
					"Object",
					attrs([("trailingComma", object.trailing_comma.to_string())]),
					vec![own, slot("CloseFodder", &object.close_fodder)],
				);
				self.fields(&format!("{path}.Fields"), &object.fields);
			}

			NodeKind::ObjectComp(comp) => {
				self.emit(
					path,
					"ObjectComp",
					attrs([("trailingComma", comp.trailing_comma.to_string())]),
					vec![
						own,
						slot("TrailingCommaFodder", &comp.trailing_comma_fodder),
						slot("CloseFodder", &comp.close_fodder),
					],
				);
				self.fields(&format!("{path}.Fields"), &comp.fields);
				self.for_spec(&format!("{path}.Spec"), &comp.spec);
			}

			NodeKind::Parens(parens) => {
				self.emit(
					path,
					"Parens",
					no_attrs(),
					vec![own, slot("CloseFodder", &parens.close_fodder)],
				);
				self.node(&format!("{path}.Inner"), &parens.inner);
			}

			NodeKind::SelfExpr => self.emit(path, "Self", no_attrs(), vec![own]),

			NodeKind::SuperIndex(super_index) => {
				self.emit(
					path,
					"SuperIndex",
					attrs([("id", ident(super_index.id.as_ref()))]),
					vec![
						own,
						slot("DotFodder", &super_index.dot_fodder),
						slot("IDFodder", &super_index.id_fodder),
					],
				);
				self.child(&format!("{path}.Index"), super_index.index.as_deref());
			}

			NodeKind::Var(id) => {
				self.emit(path, "Var", attrs([("id", id.clone())]), vec![own]);
			}

			NodeKind::Unary(unary) => {
				self.emit(
					path,
					"Unary",
					attrs([("op", unary.op.as_str().to_owned())]),
					vec![own],
				);
				self.node(&format!("{path}.Expr"), &unary.expr);
			}
		}
	}
}

/// Dump one tree the way `nodedump/dump.go`'s `dumpTree` dumps go-jsonnet's.
pub fn dump(node: &Node, final_fodder: &Fodder) -> Vec<Entry> {
	let mut dumper = Dumper::default();
	dumper.node("$", node);
	dumper.emit(
		"$$final",
		"FinalFodder",
		BTreeMap::new(),
		vec![slot("Fodder", final_fodder)],
	);
	dumper.entries
}

// ---------------------------------------------------------------------------
// Comparison.
// ---------------------------------------------------------------------------

/// The first way these two trees disagree, or `None`.
///
/// Entry by entry, and within an entry path first, then kind, then attributes,
/// then slot names, then fodder — cheapest and most structural first, so the
/// message names the shape difference rather than a consequence of it.
pub fn compare(expected: &[DumpEntry], got: &[Entry]) -> Option<String> {
	for (at, (want, have)) in expected.iter().zip(got).enumerate() {
		if want.path != have.path {
			return Some(format!(
				"entry {at}: path differs\n    go-jsonnet: {}\n    rtk:        {}",
				want.path, have.path
			));
		}
		if want.kind != have.kind {
			return Some(format!(
				"{}: kind differs: go-jsonnet {}, rtk {}",
				want.path, want.kind, have.kind
			));
		}
		if want.attrs != have.attrs {
			return Some(format!(
				"{} ({}): attributes differ\n    go-jsonnet: {:?}\n    rtk:        {:?}",
				want.path, want.kind, want.attrs, have.attrs
			));
		}

		let want_slots: Vec<&str> = want.slots.iter().map(|s| s.name.as_str()).collect();
		let have_slots: Vec<&str> = have.slots.iter().map(|s| s.0.as_str()).collect();
		if want_slots != have_slots {
			return Some(format!(
				"{} ({}): slot names differ\n    go-jsonnet: {want_slots:?}\n    rtk:        {have_slots:?}",
				want.path, want.kind
			));
		}

		for (want_slot, have_slot) in want.slots.iter().zip(&have.slots) {
			let want_fodder = describe_dumped(&want_slot.fodder);
			if want_fodder != have_slot.1 {
				return Some(format!(
					"{}.{} ({}): fodder differs\n    go-jsonnet: {want_fodder:?}\n    rtk:        {:?}",
					want.path, want_slot.name, want.kind, have_slot.1
				));
			}
		}
	}

	if expected.len() != got.len() {
		// Reported last, because a shape difference earlier in the tree is
		// more actionable than the count it produces — the loop above has
		// already checked every entry the two have in common.
		let agreed_up_to = expected
			.len()
			.min(got.len())
			.checked_sub(1)
			.and_then(|last| expected.get(last))
			.map_or_else(|| "the root".to_owned(), |entry| entry.path.clone());
		return Some(format!(
			"entry count differs: go-jsonnet {}, rtk {} (they agree up to {agreed_up_to})",
			expected.len(),
			got.len()
		));
	}

	None
}

// ---------------------------------------------------------------------------
// Paths and reporting.
// ---------------------------------------------------------------------------

pub fn crate_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn repo_root() -> PathBuf {
	crate_dir()
		.parent()
		.and_then(|crates| crates.parent())
		.expect("the crate is two levels below the repository root")
		.to_path_buf()
}

/// Report at most this many divergences in detail. Beyond a handful the list
/// stops being a thing anyone reads.
const SAMPLE: usize = 8;

pub fn report(divergences: &BTreeMap<String, String>, total: usize, what: &str) -> String {
	let mut out = format!(
		"{} of {total} {what} diverge from go-jsonnet:\n",
		divergences.len()
	);
	for (source, detail) in divergences.iter().take(SAMPLE) {
		let _ = write!(out, "\n  {source}\n    {detail}\n");
	}
	if let Some(elided) = divergences.len().checked_sub(SAMPLE).filter(|n| *n > 0) {
		let _ = write!(out, "\n  ...and {elided} more\n");
	}
	out
}

/// Read one of the generated oracles, or explain that it is missing.
///
/// Missing is a skip, not a failure: both need Go, and one needs a go-jsonnet
/// checkout staged as well.
pub fn read_oracle<T: serde::de::DeserializeOwned>(name: &str, how: &str) -> Option<T> {
	let path = crate_dir().join("testdata").join(name);
	let raw = std::fs::read_to_string(&path).ok().or_else(|| {
		eprintln!(
			"no oracle at {}; generate it with `{how}` (needs Go). Skipping.",
			path.display()
		);
		None
	})?;
	Some(
		serde_json::from_str(&raw)
			.unwrap_or_else(|err| panic!("parsing {}: {err}", path.display())),
	)
}
