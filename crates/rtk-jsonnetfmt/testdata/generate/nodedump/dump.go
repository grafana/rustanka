// The AST dumper, shared by two programs.
//
// `nodedump` (in this directory) grades rtk's *parser*: it dumps the tree
// go-jsonnet's parser produces. `passdump` (staged from ../_staged/ into a
// go-jsonnet checkout by `make update-fmt-pass-oracle`) grades rtk's
// *passes*: it dumps the tree one pass leaves behind. They have to agree about
// the notation down to the last slot name, or a Rust-side comparison against
// one could not be reused against the other — so the dumper is one file, and
// the Makefile copies it into the staged directory rather than a second copy
// being maintained here.
//
// It is `package main` in both places, which is what lets a plain `cp` do the
// sharing: the staged program cannot import this module, because
// `internal/formatter` can only be imported from inside go-jsonnet's own
// module and so the staged program has to live there.
//
// # What is deliberately not dumped
//
// Locations. `internal/formatter` reads one in exactly one place —
// `EnforceStringStyle` hands `lit.Loc()` to `StringUnescape` — and it
// **discards the error that location was for**, panicking with a fixed string
// instead. So no location is observable in the formatter's output at all, and
// dumping every `LocRange` would roughly double the oracle for nothing.
// Parse-error locations are pinned separately, by the Phase 0 fixtures in
// tests/fixtures/parse_errors/ and by the lexer oracle.
//
// Fodder is emitted structurally rather than pre-formatted, so the comparison
// notation is produced on the Rust side for both halves and no Go/Rust
// string-escaping difference can be mistaken for a fodder difference.
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"

	"github.com/google/go-jsonnet/ast"
)

// FodderElementDump mirrors ast.FodderElement, spelled exactly as the token
// oracle spells it so one Rust type deserializes both.
type FodderElementDump struct {
	Kind    string   `json:"kind"`
	Blanks  int      `json:"blanks"`
	Indent  int      `json:"indent"`
	Comment []string `json:"comment,omitempty"`
}

// Slot is one named fodder field of one node.
//
// Fodder is omitted when empty rather than written as `[]`, which costs
// nothing: the set of slot names is fixed by the node's kind, so both sides
// emit the same names in the same order and an absent array and an empty one
// mean the same thing. Emptiness is still graded — it has to be, since Slice
// treats the emptiness of StepColonFodder as meaningful.
type Slot struct {
	Name   string              `json:"name"`
	Fodder []FodderElementDump `json:"fodder,omitempty"`
}

// Entry is one node, or one of the helper structs that carries fodder of its
// own (ObjectField, Parameter, LocalBind, ForSpec and the rest).
//
// Path locates it from the root, e.g. `$.Fields[2].Expr2`. That is the whole
// point of the format: a divergence names the slot it is in.
type Entry struct {
	Path string `json:"path"`
	Kind string `json:"kind"`
	// Attrs holds what the unparser reads that is neither fodder nor a child
	// node — an operator, a field's visibility, a trailing comma. Values are
	// strings so one Rust type covers all of them; encoding/json sorts map
	// keys, so the output is deterministic.
	Attrs map[string]string `json:"attrs,omitempty"`
	Slots []Slot            `json:"slots,omitempty"`
}

// nilKind is what is recorded where something optional is absent: the kind of
// an absent child, and the value of an absent `Id` attribute.
//
// Written explicitly rather than by omitting the entry, so that a port which
// wrongly produced a node there reads as a kind mismatch at that exact path,
// and one which wrongly omitted a node reads as its opposite. Slices say their
// own length and get no such marker.
const nilKind = "<nil>"

func fodderKindName(kind ast.FodderKind) string {
	switch kind {
	case ast.FodderLineEnd:
		return "LineEnd"
	case ast.FodderInterstitial:
		return "Interstitial"
	case ast.FodderParagraph:
		return "Paragraph"
	default:
		return "unknown"
	}
}

func dumpFodder(fodder ast.Fodder) []FodderElementDump {
	if len(fodder) == 0 {
		return nil
	}
	out := make([]FodderElementDump, 0, len(fodder))
	for _, element := range fodder {
		out = append(out, FodderElementDump{
			Kind:    fodderKindName(element.Kind),
			Blanks:  element.Blanks,
			Indent:  element.Indent,
			Comment: element.Comment,
		})
	}
	return out
}

func slot(name string, fodder ast.Fodder) Slot {
	return Slot{Name: name, Fodder: dumpFodder(fodder)}
}

func fieldKindName(kind ast.ObjectFieldKind) string {
	switch kind {
	case ast.ObjectAssert:
		return "ObjectAssert"
	case ast.ObjectFieldID:
		return "ObjectFieldID"
	case ast.ObjectFieldExpr:
		return "ObjectFieldExpr"
	case ast.ObjectFieldStr:
		return "ObjectFieldStr"
	case ast.ObjectLocal:
		return "ObjectLocal"
	default:
		return "unknown"
	}
}

func hideName(hide ast.ObjectFieldHide) string {
	switch hide {
	case ast.ObjectFieldHidden:
		return "Hidden"
	case ast.ObjectFieldInherit:
		return "Inherit"
	case ast.ObjectFieldVisible:
		return "Visible"
	default:
		return "unknown"
	}
}

func stringKindName(kind ast.LiteralStringKind) string {
	switch kind {
	case ast.StringSingle:
		return "StringSingle"
	case ast.StringDouble:
		return "StringDouble"
	case ast.StringBlock:
		return "StringBlock"
	case ast.VerbatimStringDouble:
		return "VerbatimStringDouble"
	case ast.VerbatimStringSingle:
		return "VerbatimStringSingle"
	default:
		return "unknown"
	}
}

func identName(id *ast.Identifier) string {
	if id == nil {
		return nilKind
	}
	return string(*id)
}

type dumper struct {
	entries []Entry
}

func (d *dumper) emit(path, kind string, attrs map[string]string, slots ...Slot) {
	d.entries = append(d.entries, Entry{Path: path, Kind: kind, Attrs: attrs, Slots: slots})
}

// child walks an optional child, recording an explicit marker when it is
// absent.
func (d *dumper) child(path string, node ast.Node) {
	if node == nil {
		d.emit(path, nilKind, nil)
		return
	}
	d.node(path, node)
}

func index(path string, i int) string {
	return path + "[" + strconv.Itoa(i) + "]"
}

// params walks a parameter list. The parameters of a Function node, of a field
// method and of a local bind's Fun all reach the unparser through
// unparseParams, so they are dumped the same way wherever they came from.
func (d *dumper) params(path string, params []ast.Parameter) {
	for i, param := range params {
		at := index(path, i)
		d.emit(at, "Parameter", map[string]string{"name": string(param.Name)},
			slot("NameFodder", param.NameFodder),
			slot("EqFodder", param.EqFodder),
			slot("CommaFodder", param.CommaFodder))
		d.child(at+".DefaultArg", param.DefaultArg)
	}
}

// method walks the *Function hanging off an object field or a local bind.
//
// It carries no opening fodder of its own — there was no `function` keyword —
// which is why it is not walked as an ordinary node: doing so would invent a
// `Fodder` slot that the unparser never reads and the parser never fills.
func (d *dumper) method(path string, fun *ast.Function) {
	if fun == nil {
		d.emit(path, nilKind, nil)
		return
	}
	d.emit(path, "Method",
		map[string]string{"trailingComma": strconv.FormatBool(fun.TrailingComma)},
		slot("ParenLeftFodder", fun.ParenLeftFodder),
		slot("ParenRightFodder", fun.ParenRightFodder))
	d.params(path+".Parameters", fun.Parameters)
}

// forSpec walks a comprehension specification, outermost first, the way
// unparseSpecs recurses into Outer before writing anything of its own.
func (d *dumper) forSpec(path string, spec *ast.ForSpec) {
	if spec.Outer != nil {
		d.forSpec(path+".Outer", spec.Outer)
	}
	d.emit(path, "ForSpec", map[string]string{"varName": string(spec.VarName)},
		slot("ForFodder", spec.ForFodder),
		slot("VarFodder", spec.VarFodder),
		slot("InFodder", spec.InFodder))
	d.node(path+".Expr", spec.Expr)
	for i, cond := range spec.Conditions {
		at := index(path+".Conditions", i)
		d.emit(at, "IfSpec", nil, slot("IfFodder", cond.IfFodder))
		d.node(at+".Expr", cond.Expr)
	}
}

func (d *dumper) fields(path string, fields ast.ObjectFields) {
	for i, field := range fields {
		at := index(path, i)
		d.emit(at, "ObjectField", map[string]string{
			"fieldKind":  fieldKindName(field.Kind),
			"hide":       hideName(field.Hide),
			"superSugar": strconv.FormatBool(field.SuperSugar),
			"id":         identName(field.Id),
		},
			slot("Fodder1", field.Fodder1),
			slot("Fodder2", field.Fodder2),
			slot("OpFodder", field.OpFodder),
			slot("CommaFodder", field.CommaFodder))
		d.method(at+".Method", field.Method)
		d.child(at+".Expr1", field.Expr1)
		d.child(at+".Expr2", field.Expr2)
		d.child(at+".Expr3", field.Expr3)
	}
}

func (d *dumper) binds(path string, binds ast.LocalBinds) {
	for i, bind := range binds {
		at := index(path, i)
		d.emit(at, "LocalBind", map[string]string{"variable": string(bind.Variable)},
			slot("VarFodder", bind.VarFodder),
			slot("EqFodder", bind.EqFodder),
			slot("CloseFodder", bind.CloseFodder))
		d.method(at+".Fun", bind.Fun)
		d.child(at+".Body", bind.Body)
	}
}

func (d *dumper) commaSeparated(path string, exprs []ast.CommaSeparatedExpr) {
	for i, element := range exprs {
		at := index(path, i)
		d.emit(at, "CommaSeparatedExpr", nil, slot("CommaFodder", element.CommaFodder))
		d.child(at+".Expr", element.Expr)
	}
}

// node walks one AST node: its own entry first, carrying NodeBase.Fodder and
// every fodder slot of its type, then its children in the order the unparser
// writes them.
//
// NodeBase.Fodder is dumped for every node because its *emptiness* is
// load-bearing: a left-recursive node's opening fodder is stored as far inside
// the tree as possible, so on a Binary or an Index it is nil and the fodder is
// on the leftmost leaf instead. A port that put it on the outer node would
// still round-trip most files and would be wrong here, at this path.
func (d *dumper) node(path string, expr ast.Node) {
	base := expr.OpenFodder()
	own := slot("Fodder", *base)

	switch node := expr.(type) {
	case *ast.Apply:
		d.emit(path, "Apply", map[string]string{
			"trailingComma": strconv.FormatBool(node.TrailingComma),
			"tailStrict":    strconv.FormatBool(node.TailStrict),
		}, own,
			slot("FodderLeft", node.FodderLeft),
			slot("FodderRight", node.FodderRight),
			slot("TailStrictFodder", node.TailStrictFodder))
		d.node(path+".Target", node.Target)
		d.commaSeparated(path+".Arguments.Positional", node.Arguments.Positional)
		for i, arg := range node.Arguments.Named {
			at := index(path+".Arguments.Named", i)
			d.emit(at, "NamedArgument", map[string]string{"name": string(arg.Name)},
				slot("NameFodder", arg.NameFodder),
				slot("EqFodder", arg.EqFodder),
				slot("CommaFodder", arg.CommaFodder))
			d.child(at+".Arg", arg.Arg)
		}

	case *ast.ApplyBrace:
		d.emit(path, "ApplyBrace", nil, own)
		d.node(path+".Left", node.Left)
		d.node(path+".Right", node.Right)

	case *ast.Array:
		d.emit(path, "Array",
			map[string]string{"trailingComma": strconv.FormatBool(node.TrailingComma)},
			own, slot("CloseFodder", node.CloseFodder))
		d.commaSeparated(path+".Elements", node.Elements)

	case *ast.ArrayComp:
		d.emit(path, "ArrayComp",
			map[string]string{"trailingComma": strconv.FormatBool(node.TrailingComma)},
			own,
			slot("TrailingCommaFodder", node.TrailingCommaFodder),
			slot("CloseFodder", node.CloseFodder))
		d.node(path+".Body", node.Body)
		d.forSpec(path+".Spec", &node.Spec)

	case *ast.Assert:
		d.emit(path, "Assert", nil, own,
			slot("ColonFodder", node.ColonFodder),
			slot("SemicolonFodder", node.SemicolonFodder))
		d.node(path+".Cond", node.Cond)
		d.child(path+".Message", node.Message)
		d.node(path+".Rest", node.Rest)

	case *ast.Binary:
		d.emit(path, "Binary", map[string]string{"op": node.Op.String()},
			own, slot("OpFodder", node.OpFodder))
		d.node(path+".Left", node.Left)
		d.node(path+".Right", node.Right)

	case *ast.Conditional:
		d.emit(path, "Conditional", nil, own,
			slot("ThenFodder", node.ThenFodder),
			slot("ElseFodder", node.ElseFodder))
		d.node(path+".Cond", node.Cond)
		d.node(path+".BranchTrue", node.BranchTrue)
		d.child(path+".BranchFalse", node.BranchFalse)

	case *ast.Dollar:
		d.emit(path, "Dollar", nil, own)

	case *ast.Error:
		d.emit(path, "Error", nil, own)
		d.node(path+".Expr", node.Expr)

	case *ast.Function:
		d.emit(path, "Function",
			map[string]string{"trailingComma": strconv.FormatBool(node.TrailingComma)},
			own,
			slot("ParenLeftFodder", node.ParenLeftFodder),
			slot("ParenRightFodder", node.ParenRightFodder))
		d.params(path+".Parameters", node.Parameters)
		d.node(path+".Body", node.Body)

	case *ast.Import:
		d.emit(path, "Import", nil, own)
		d.node(path+".File", node.File)

	case *ast.ImportStr:
		d.emit(path, "ImportStr", nil, own)
		d.node(path+".File", node.File)

	case *ast.ImportBin:
		d.emit(path, "ImportBin", nil, own)
		d.node(path+".File", node.File)

	case *ast.Index:
		// RightBracketFodder is the fodder before the ']' when Index is used
		// and the fodder before the id when Id is used. One slot, two jobs —
		// which is why the AST has to be ported in this shape rather than
		// given a tidier one, and why PrettyFieldNames assigning over it
		// silently drops a comment.
		d.emit(path, "Index", map[string]string{"id": identName(node.Id)},
			own,
			slot("LeftBracketFodder", node.LeftBracketFodder),
			slot("RightBracketFodder", node.RightBracketFodder))
		d.node(path+".Target", node.Target)
		d.child(path+".Index", node.Index)

	case *ast.Slice:
		d.emit(path, "Slice", nil, own,
			slot("LeftBracketFodder", node.LeftBracketFodder),
			slot("EndColonFodder", node.EndColonFodder),
			// The unparser writes the step colon when Step is set *or* this is
			// non-empty, so an empty one here is not the same as an absent
			// one and both have to survive the port. NoRedundantSliceColon is
			// the pass that empties it.
			slot("StepColonFodder", node.StepColonFodder),
			slot("RightBracketFodder", node.RightBracketFodder))
		d.node(path+".Target", node.Target)
		d.child(path+".BeginIndex", node.BeginIndex)
		d.child(path+".EndIndex", node.EndIndex)
		d.child(path+".Step", node.Step)

	case *ast.InSuper:
		d.emit(path, "InSuper", nil, own,
			slot("InFodder", node.InFodder),
			slot("SuperFodder", node.SuperFodder))
		d.node(path+".Index", node.Index)

	case *ast.Local:
		d.emit(path, "Local", nil, own)
		d.binds(path+".Binds", node.Binds)
		d.node(path+".Body", node.Body)

	case *ast.LiteralBoolean:
		d.emit(path, "LiteralBoolean",
			map[string]string{"value": strconv.FormatBool(node.Value)}, own)

	case *ast.LiteralNull:
		d.emit(path, "LiteralNull", nil, own)

	case *ast.LiteralNumber:
		// OriginalString, not a parsed value: the unparser writes it back
		// verbatim, so `1.0` and `1.000` are different programs to the
		// formatter even though they are the same number.
		d.emit(path, "LiteralNumber",
			map[string]string{"originalString": node.OriginalString}, own)

	case *ast.LiteralString:
		d.emit(path, "LiteralString", map[string]string{
			"kind": stringKindName(node.Kind),
			// Still carrying its original escape sequences for the two fully
			// escaped kinds, and already unescaped for the verbatim ones.
			"value":           node.Value,
			"blockIndent":     node.BlockIndent,
			"blockTermIndent": node.BlockTermIndent,
		}, own)

	case *ast.Object:
		d.emit(path, "Object",
			map[string]string{"trailingComma": strconv.FormatBool(node.TrailingComma)},
			own, slot("CloseFodder", node.CloseFodder))
		d.fields(path+".Fields", node.Fields)

	case *ast.ObjectComp:
		d.emit(path, "ObjectComp",
			map[string]string{"trailingComma": strconv.FormatBool(node.TrailingComma)},
			own,
			slot("TrailingCommaFodder", node.TrailingCommaFodder),
			slot("CloseFodder", node.CloseFodder))
		d.fields(path+".Fields", node.Fields)
		d.forSpec(path+".Spec", &node.Spec)

	case *ast.Parens:
		d.emit(path, "Parens", nil, own, slot("CloseFodder", node.CloseFodder))
		d.node(path+".Inner", node.Inner)

	case *ast.Self:
		d.emit(path, "Self", nil, own)

	case *ast.SuperIndex:
		// IDFodder is the fodder before the 'f' in super.f and before the ']'
		// in super[e]; DotFodder is the fodder before the '.' or the '['. The
		// same doubling-up as Index, with different names.
		d.emit(path, "SuperIndex", map[string]string{"id": identName(node.Id)},
			own,
			slot("DotFodder", node.DotFodder),
			slot("IDFodder", node.IDFodder))
		d.child(path+".Index", node.Index)

	case *ast.Var:
		d.emit(path, "Var", map[string]string{"id": string(node.Id)}, own)

	case *ast.Unary:
		d.emit(path, "Unary", map[string]string{"op": node.Op.String()}, own)
		d.node(path+".Expr", node.Expr)

	default:
		// Loud rather than silent: a node kind this does not know about is a
		// hole in the oracle, and a hole in the oracle is worse than a red
		// test because nothing downstream would report it.
		panic(fmt.Sprintf("nodedump: unhandled AST node %T at %s", expr, path))
	}
}

// dumpTree walks a whole file: the root, then the fodder after its last token.
func dumpTree(node ast.Node, finalFodder ast.Fodder) []Entry {
	d := &dumper{}
	d.node("$", node)
	// The fodder after the last token of the file, which is what fillFinal and
	// removeExtraTrailingNewlines consume. It belongs to no node, so it gets a
	// path no node can collide with.
	d.emit("$$final", "FinalFodder", nil, slot("Fodder", finalFodder))
	return d.entries
}

// Manifest is the corpus manifest, read so these oracles and the corpus always
// grade the same list of files.
type Manifest struct {
	Entries []struct {
		Source string `json:"source"`
	} `json:"entries"`
}

// Snippet is one entry of a snippets file. `why` documents the case for human
// readers and is ignored here.
type Snippet struct {
	Name   string `json:"name"`
	Source string `json:"source"`
}

func readJSON(path string, into any) error {
	raw, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	return json.Unmarshal(raw, into)
}

func writeJSON(path string, value any) error {
	encoded, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, append(encoded, '\n'), 0o644)
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, err)
	os.Exit(1)
}
