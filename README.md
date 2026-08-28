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

## Feature compatibility with Tanka

Status is relative to [Tanka](https://github.com/grafana/tanka) (`tk`). Commands that
exist in the CLI but currently exit with `not implemented` are marked **not implemented**.

### Workflow commands

| Feature | `tk` | `rtk` | Notes |
| --- | --- | --- | --- |
| `apply` | yes | yes | |
| `show` | yes | yes | |
| `diff` | yes | yes | |
| `prune` | yes | yes | |
| `export` | yes | yes | |
| `eval` | yes | yes | |
| `lint` | yes | yes | `rtk lint` uses jrsonnet-lint (`--fix`, `--disable-checks`) |
| `env add` / `list` / `remove` / `set` | yes | yes | |
| `delete` | yes | no | Tracking: [#14](https://github.com/grafana/rustanka/issues/14) |
| `status` | yes | no | Tracking: [#13](https://github.com/grafana/rustanka/issues/13) |
| `fmt` | yes | no | Tracking: [#11](https://github.com/grafana/rustanka/issues/11) |
| `init` | yes | no | Tracking: [#12](https://github.com/grafana/rustanka/issues/12) |
| `complete` | yes | no | Tracking: [#15](https://github.com/grafana/rustanka/issues/15) |

### Tools

| Feature | `tk` | `rtk` | Notes |
| --- | --- | --- | --- |
| `tool imports` | yes | yes | |
| `tool imports --check` | yes | no | Tracking: [#17](https://github.com/grafana/rustanka/issues/17) |
| `tool importers` | yes | yes | |
| `tool charts` (`init`, `add`, `add-repo`, `vendor`, `config`, `version-check`) | yes | yes | |
| `tool jpath` | yes | no | Tracking: [#16](https://github.com/grafana/rustanka/issues/16) |
| `tool importers-count` | yes | yes | |

### Jsonnet natives and related features

| Feature | `tk` | `rtk` | Notes |
| --- | --- | --- | --- |
| Helm templating (`std.native('helmTemplate')`) | yes | yes | |
| Helm chart vendoring (`tool charts`) | yes | yes | |
| Kustomize (`std.native('kustomizeBuild')`) | yes | yes | |
| `--jsonnet-implementation` (`go`, `binary:…`, `c++`, `reference`) | yes | no | Always uses built-in jrsonnet. Tracking: [#23](https://github.com/grafana/rustanka/issues/23), [#33](https://github.com/grafana/rustanka/issues/33), [#34](https://github.com/grafana/rustanka/issues/34) |
| `spec.exportJsonnetImplementation` | yes | n/a | No-op: evaluation is always jrsonnet |
| jsonnet-bundler (`jb`) | external | external | Planned as `rtk bundler`: [#27](https://github.com/grafana/rustanka/issues/27) |

### Rustanka-only / planned

| Feature | `tk` | `rtk` | Notes |
| --- | --- | --- | --- |
| `validate` | no | yes | Policy checks against exported manifests |
| `helm` | no | no | Helm-like CLI wrapper. Tracking: [#26](https://github.com/grafana/rustanka/issues/26) |
| `bundler` | no | no | Built-in replacement for `jb`. Tracking: [#27](https://github.com/grafana/rustanka/issues/27) |
