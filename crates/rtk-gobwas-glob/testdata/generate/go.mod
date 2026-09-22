// Not part of any build. Regenerates ../gobwas-glob-v0.2.3.json:
//
//     cd crates/rtk-gobwas-glob/testdata/generate
//     go mod tidy && go run . > ../gobwas-glob-v0.2.3.json
//
// Pin this to whatever version of the library Tanka links; the answers in the
// table are only meaningful for that one. `go mod tidy` writes the go.sum.
module generate

go 1.21

require github.com/gobwas/glob v0.2.3
