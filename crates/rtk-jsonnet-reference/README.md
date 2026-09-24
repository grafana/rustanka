# Reference Jsonnet runtime binding

Rust-only bindings to the **published C API** of the reference C++ Jsonnet interpreter, loaded on demand with `libloading`. No C++ shim or build-time link is required. `Implementation::new()` loads `libjsonnet.so.0` on Linux (or the corresponding platform library); `RTK_JSONNET_REFERENCE_LIBRARY` overrides its path. Only `v0.22.0` is accepted. Cloned implementations retain the library while their evaluators use it.

This crate implements `rtk_jsonnet_core::Implementation`, `Evaluator`, `Value`, native function and serde interfaces. Evaluation uses a VM configured with import paths, external variables/code, top-level arguments/code and optional stack depth. Native functions receive eager primitive arguments and can return JSON values (including arrays and objects); callback errors are returned as Jsonnet runtime errors. Panics are caught before crossing the C ABI.

Import paths are given in order of precedence, first wins, as they are to every implementation. libjsonnet searches the path it was given *last* first, so they are handed to it reversed.

## C API limitations

The public `jsonnet_evaluate_*` functions return fully manifested JSON, not interpreter values. **Every visible field is evaluated before any Rust field lookup**; a hidden field is not returned and cannot be inspected, even with `Hidden::Include`. Object values therefore do not have the lazy or hidden-field behavior provided by Jrsonnet. The JSON-backed `Value::manifest` also serializes the parsed value, rather than retaining the C library's exact original output formatting. These are important differences for Tanka-compatible output.

The C library only passes primitive arguments to native callbacks: passing an array or object to `std.native` fails in libjsonnet, before the callback, with `native extensions can only take primitives`. Its native registration API also has no optional parameters; attempts to register a core function with optional arguments return an error. Reference-specific implementation flags are not supported and return an error.

In practice this means:

- **`helmTemplate` and `kustomizeBuild` cannot be used.** Both take their configuration as an object, so an environment that renders a Helm chart or a Kustomization cannot be evaluated by this backend. Natives taking only strings and numbers (`sha256`, `parseYaml`, `regexMatch`, …) work.
- **`rtkMemoize` is eager and primitive-only.** Its value is forced before the call, so nothing is saved; what is kept is that the first value stored under a key is the one every later call receives, process-wide, as with Jrsonnet.
- **Strings crossing a native call cannot contain NUL.** The C API passes C strings, so an argument is truncated at its first NUL and a result containing one is an error. Evaluation output is unaffected.

## Where it is available

The library is a runtime dependency, so a build of rtk that can use this backend is not enough on its own:

- The published container image (distroless) does not ship libjsonnet.
- A fully static binary (for example a musl build) cannot `dlopen` at all.
- Distribution packages are frequently older than `v0.22.0` (Ubuntu 24.04 and Debian 13 ship 0.20.0) and are refused. Building `libjsonnet.so` from the upstream tag is quick; CI does exactly that, see `.github/workflows/test.yaml`.

## Selection

The parent engine selects this backend for `--jsonnet-implementation c++` (or `reference`), a project's `tkrc.yaml` choice, or an environment's `exportJsonnetImplementation: c++` when no project choice overrides it. A missing or incompatible library fails explicitly rather than falling back to Jrsonnet.

## Tests

Tests that need the library skip when it is missing or of another version, so that an ordinary machine can run the suite. Set `RTK_REQUIRE_REFERENCE_JSONNET=1` to make them fail instead; CI does, so a green run means they ran.
