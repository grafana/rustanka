// Used by the Rust parity test to generate reference answers at runtime.
// Run `go run -mod=readonly .` here to inspect the answers manually.
// Keep the gobwas/glob version pinned to the version Tanka links.
module generate

go 1.21

require github.com/gobwas/glob v0.2.3
