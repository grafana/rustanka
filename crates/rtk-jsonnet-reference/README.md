# Reference Jsonnet runtime binding

Rust-only bindings to the **published C API** of the reference C++ Jsonnet interpreter, loaded on demand with `libloading`. No C++ shim or build-time link is required. `Implementation::new()` loads `libjsonnet.so.0` on Linux (or the corresponding platform library); `RTK_JSONNET_REFERENCE_LIBRARY` overrides its path. Only `v0.22.0` is accepted. Cloned implementations retain the library while their evaluators use it.

This crate implements `rtk_jsonnet_core::Implementation`, `Evaluator`, `Value`, native function and serde interfaces. Evaluation uses a VM configured with import paths, external variables/code, top-level arguments/code and optional stack depth. Native functions receive eager primitive arguments and can return JSON values (including arrays and objects); callback errors are returned as Jsonnet runtime errors. Panics are caught before crossing the C ABI.

## C API limitations

The public `jsonnet_evaluate_*` functions return fully manifested JSON, not interpreter values. **Every visible field is evaluated before any Rust field lookup**; a hidden field is not returned and cannot be inspected, even with `Hidden::Include`. Object values therefore do not have the lazy or hidden-field behavior provided by Jrsonnet. The JSON-backed `Value::manifest` also serializes the parsed value, rather than retaining the C library's exact original output formatting. These are important differences for Tanka-compatible output.

The C library only passes primitive arguments to native callbacks: passing an array or object to `std.native` fails in libjsonnet before the callback. Its native registration API also has no optional parameters; attempts to register a core function with optional arguments return an error. `rtkMemoize` is consequently eager and only works with primitive values, unlike the lazy Jrsonnet implementation. Reference-specific implementation flags are not supported and return an error.

The parent engine selects this backend for `--jsonnet-implementation c++` (or `reference`), a project's `tkrc.yaml` choice, or an environment's `exportJsonnetImplementation: c++` when no project choice overrides it. A missing or incompatible library fails explicitly rather than falling back to Jrsonnet.
