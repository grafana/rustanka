<p align="center">
  <img
    width="200"
    src="./docs/rustanka.svg"
    alt="Rustanka Logo"
  />
</p>

# Rustanka

<img
  src="https://raw.githubusercontent.com/grafana/tanka/main/docs/img/example.png"
  width="50%"
  align="right"
/>

**The clean, concise and super flexible alternative to YAML for your
[Kubernetes](https://k8s.io) cluster — now in Rust, and faster than ever before 🚀**

Rustanka (`rtk`) is a drop-in replacement for [Tanka](https://github.com/grafana/tanka)
(`tk`). It has replaced Tanka in production at Grafana Labs.

- **✨ Clean**: The
  [Jsonnet language](https://jsonnet.org) expresses your apps more obviously than YAML ever did.
- **🏗️ Reusable**: Build libraries, import them anytime and even share them on GitHub!
- **📌 Concise**: Using the Kubernetes library and abstraction, you will
  never see boilerplate again!
- **🎯 Confidence**: Stop guessing and use `rtk diff` to see what exactly will happen.
- **🔭 Helm**: Vendor in, modify, and export [Helm charts reproducibly](https://tanka.dev/helm#helm-support).

## Performance vs Tanka

`rtk` vs `tk` on Grafana Labs' CI benchmarks ([sample run](https://github.com/grafana/rustanka/pull/38)).
Faster in every case, up to:

| Command | Up to | Notes |
| --- | --- | --- |
| `eval` | 41× faster | Uses jrsonnet directly, which is much faster than go-jsonnet |
| `export` (full) | 25× faster | |
| `export` (replace) | 32× faster | |
| `diff` | 51× faster | kubectl operations have been replaced with native Kubernetes API calls |
| `tool importers` | 8× faster | |

## Feature compatibility with Tanka

Status is relative to [Tanka](https://github.com/grafana/tanka) (`tk`). ✅ implemented,
❌ not implemented (including CLI stubs that exit with `not implemented`), ➖ not applicable.

### Workflow commands

| Feature | `tk` | `rtk` | Notes |
| --- | --- | --- | --- |
| `apply` | ✅ | ✅ | |
| `show` | ✅ | ✅ | |
| `diff` | ✅ | ✅ | |
| `prune` | ✅ | ✅ | |
| `export` | ✅ | ✅ | |
| `eval` | ✅ | ✅ | |
| `lint` | ✅ | ✅ | `rtk lint` uses jrsonnet-lint (`--fix`, `--disable-checks`) |
| `env add` / `list` / `remove` / `set` | ✅ | ✅ | |
| `delete` | ✅ | ❌ | Tracking: [#14](https://github.com/grafana/rustanka/issues/14) |
| `status` | ✅ | ❌ | Tracking: [#13](https://github.com/grafana/rustanka/issues/13) |
| `fmt` | ✅ | ❌ | Tracking: [#11](https://github.com/grafana/rustanka/issues/11) |
| `init` | ✅ | ❌ | Tracking: [#12](https://github.com/grafana/rustanka/issues/12) |
| `complete` | ✅ | ❌ | Tracking: [#15](https://github.com/grafana/rustanka/issues/15) |

### Tools

| Feature | `tk` | `rtk` | Notes |
| --- | --- | --- | --- |
| `tool imports` | ✅ | ✅ | |
| `tool imports --check` | ✅ | ❌ | Tracking: [#17](https://github.com/grafana/rustanka/issues/17) |
| `tool importers` | ✅ | ✅ | |
| `tool charts` (`init`, `add`, `add-repo`, `vendor`, `config`, `version-check`) | ✅ | ✅ | |
| `tool jpath` | ✅ | ❌ | Tracking: [#16](https://github.com/grafana/rustanka/issues/16) |
| `tool importers-count` | ✅ | ✅ | |

### Jsonnet natives and related features

| Feature | `tk` | `rtk` | Notes |
| --- | --- | --- | --- |
| Helm templating (`std.native('helmTemplate')`) | ✅ | ✅ | |
| Helm chart vendoring (`tool charts`) | ✅ | ✅ | |
| Kustomize (`std.native('kustomizeBuild')`) | ✅ | ✅ | |
| `--jsonnet-implementation` (`go`, `binary:…`, `c++`, `reference`) | ✅ | ❌ | Always uses built-in jrsonnet. Tracking: [#23](https://github.com/grafana/rustanka/issues/23), [#33](https://github.com/grafana/rustanka/issues/33), [#34](https://github.com/grafana/rustanka/issues/34) |
| `spec.exportJsonnetImplementation` | ✅ | ➖ | No-op: evaluation is always jrsonnet |
| jsonnet-bundler (`jb`) | ➖ | ➖ | External tool. Planned as `rtk bundler`: [#27](https://github.com/grafana/rustanka/issues/27) |

### Rustanka-only / planned

| Feature | `tk` | `rtk` | Notes |
| --- | --- | --- | --- |
| `validate` | ❌ | ✅ | Policy checks against exported manifests |
| `helm` | ❌ | ❌ | Helm-like CLI wrapper. Tracking: [#26](https://github.com/grafana/rustanka/issues/26) |
| `bundler` | ❌ | ❌ | Built-in replacement for `jb`. Tracking: [#27](https://github.com/grafana/rustanka/issues/27) |
