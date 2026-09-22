// Not part of any build. Regenerates ../corpus/ and ../go-jsonnet-corpus.json:
//
//     cd crates/rtk-jsonnetfmt/testdata/generate
//     go mod tidy && go run . <repo root> <corpus dir> <go-jsonnet checkout> <external corpus file>
//
// or, from the repo root, `make update-fmt-corpus`, which resolves the
// checkout — and which asserts that the version required below is the one it
// clones, since the two sets have to answer for the same go-jsonnet.
//
// `go mod tidy` pinned go-jsonnet v0.22.0, which is the version the committed
// goldens came from; the generated manifest.json records it too, read from the
// build info at runtime. Leave it pinned — an upstream release must not be able
// to move the goldens without a visible diff here.
module generate

go 1.24.5

require github.com/google/go-jsonnet v0.22.0
