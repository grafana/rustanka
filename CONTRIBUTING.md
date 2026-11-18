# Contributing to Tanka

## Overview

The codebase you're looking at now is Rustanka, the Rust replacement for [the
original implementation of Grafana Tanka][go-tanka].

The explicit goal of this project is two-fold, in this order:
- Create a completely backwards-compatible implementation of
  [Grafana Tanka][tanka].
- Create a better experience than [Go Tanka][go-tanka]- both in terms of
  performance and features.

Essentially- If something creates a better experience than Grafana Tanka, but
isn't backwards compatible- it will not be accepted. Otherwise, feel free to
hack on the codebase and improve your Kubernetes as-code experience 🛠️.

### Codebase Layout

When running `ls` on the root directory of this repository, you'll get the
following:
```sh
$ ls
AGENTS.md   CLAUDE.md   CONTRIBUTING.md      docs           README.md              
target      xtask       Cargo.lock           clippy.toml    crates               
LICENSE     rtk         tests                Cargo.toml     cmds
Dockerfile  Makefile    rust-toolchain.toml  trivy.yaml
```

This may seem a little cluttered- but there's good reason- in order to maintain
strict backwards-compatibility with Go Tanka, Rustanka had to vendor in a fork
of [Jrsonnet][jrsonnet]. As a result, if you compare the layout of Rustankas
repository to the layout of Jrsonnets repository, you'll notice many
similarities.

By convention, Rustanka-specific code should land in:

- `crates/rtk-${crate name}/` for new crates.

- `cmds/rtk-${command name}/` for new commands.

- `rtk/` for other new files, such as CI tests.

### Code Style

While much of this codebase uses a very relaxed Go-esq coding style, new
contributions should stringently follow Rust best practices, and an effort
should be made to improve the existing coding style until it is the norm-
see [issue #18](https://github.com/grafana/rustanka/issues/18).

Axioms of the preferred coding style include:

- Always running `cargo fmt`/`make fmt`.

- Always running `cargo clippy` on new code.

- Always following Rust idioms.

- Leaning towards lazy-evaluation where possible. 

- Avoidance of unnecesary allocations.
 
- Avoidance of bare/unassociated functions.

## Setting Up Your Development Environment

To hack on Rustanka, you'll need the following dependencies installed:

- <details><summary>**A Unix-like execution environment**</summary>
  - Linux and MacOS have this out of the box.
  - On Windows, this either requires the use of WSL/WSL2 or a tool like Msys2. WSL is ultimately much simpler to use, but means that you will be unable to (easily) make native Windows binaries.
  </details>

- <details><summary>**Git VCS**</summary>
  - On Linux, install from your distributions packages- it should be called
    `git`.
  - On MacOS, install from [Brew][brew]: `brew install git`.
  - On Windows, this can be installed:
    - Via WSL/WSL2 using the installed distributions packages- it should be
      called `git`.
    - Via Msys2: `pacman -Syu git` in the Msys2 shell.
  </details>

- <details><summary>**GNU Make**</summary>
  - On Linux, install from your distributions packages- it should be called
    `make`.
  - On MacOS, install from [Brew][brew]: `brew install make`.
  - On Windows, this can be installed:
    - Via WSL/WSL2 using the installed distributions packages- it should be
      called `make`.
    - Via Msys2: `pacman -Syu make` in the Msys2 shell.
  </details>

- <details><summary>A viable **C/C++ toolchain**</summary>
  - On Linux, install from your distributions packages- Clang should be
    preferred over GCC, which should be called `clang` on most ditributions.
  - On MacOS, this can be achieved by running `gcc` in your terminal- it will prompt you to install the XCode developer toolchain. The CLI interface is that of `gcc`, but it will actually use `clang` under the hood. 
  - On Windows:
    - For WSL/WSL2 it can be installed via the distributions packages- it should be called `clang` on most distributions.
    - For Msys2, you'll both need to install [Microsoft Visual Studio](https://visualstudio.microsoft.com/) and select `C/C++ Clang Compiler for Windows` during installation, as well as running `pacman -Syu mingw-w64-$(arch)-clang` in the Msys2 shell.
  </details>

- <details><summary>**The Go Programming Language**</summary>
  - On Linux, install from your distributions packages- it should be called `go`
    or `golang` on most distributions.
  - On MacOS, install from [Brew][brew]: `brew install go`.
  - On Windows:
    - Via WSL/WSL2 using the installed distributions packages- it should be
      called `go` or `golang`.
    - Via Msys2: `pacman -Syu mingw-w64-go`.
  </details>

- **The Rust Programming Language**: See [rustup.rs](https://rustup.rs).

- <details><summary>**The `kubectl` CLI**</summary>
  - On Linux, install from your distributions packages- it should be called
    `kubectl`.
  - On MacOS, install from [Brew][brew]: `brew install kubectl`.
  - On Windows:
    - Via WSL/WSL2 using the installed distributions packages- it should be
      called `kubectl`.
    - Via Msys2: `winget install -e --id Kubernetes.kubectl`.
  </details>

- <details><summary>**The `kustomize` CLI**</summary>
  - On Linux, install from your distributions packages- it should be called
    `kustomize`.
  - On MacOS, install from [Brew][brew]: `brew install kustomize`.
  - On Windows:
    - Via WSL/WSL2 using the installed distributions packages- it should be
      called `helm`.
    - Via Msys2: `winget install Kubernetes.kustomize`.
  </details>

- <details><summary>**The `helm` CLI**</summary>
  - On Linux, install from your distributions packages- it should be called
    `helm`.
  - On MacOS, install from [Brew][brew]: `brew install helm`.
  - On Windows:
    - Via WSL/WSL2 using the installed distributions packages- it should be
      called `helm`.
    - Via Msys2: `winget install Helm.Helm`.
  </details>

- <details><summary>**The C++/Reference Jsonnet Implementation**</summary>
  - On Linux, install from your distributions packages- it should be called
    `jsonnet`, though you'll also need the headers, which may be a seperate
    package with a `-dev` or `-devel` suffix.
  - On MacOS, install from [Brew][brew]: `brew install jsonnet`.
  - On Windows:
    - Via WSL/WSL using the installed distributions packages- it should be
      called `jsonnet`, though you'll also need the headers, which may be a
      seperate package with a `-dev` or `-devel` suffix.
    - On Msys2, you'll have to build and install Jsonnet from source:
      https://github.com/google/jsonnet.
  </details>

- **Go Tanka**: `go install github.com/grafana/tanka/cmd/tk@latest`.

If you're on one of the following systems, you can use these provided one-liners
to install all necessary dependencies:

- <details><summary>**Linux Distributions**</summary>
    <details><summary>**Debian-based Linux Distributions (Including Ubuntu!)**</summary>
    ```sh
    sudo apt update && \
      sudo apt install -y clang curl git golang-go helm jsonnet libjsonnet-dev kubectl kustomize make && \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh && \
      go install github.com/grafana/tanka/cmd/tk@latest
    ```
    </details>
    <details><summary>**Red-Hat-based Linux Distributions (Including Fedora!)**</summary>
    ```sh
    sudo yum install -y clang curl git golang helm jsonnet-devel kubectl kustomize make && \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh && \
      go install github.com/grafana/tanka/cmd/tk@latest
    ```
    </details>
    <details><summary>**Arch-based Linux Distributions**</summary>
    ```sh
    yes | sudo pacman -Syu base-devel clang curl git go helm kubectl kustomize make && \
      pushd /tmp && \
      git clone 'https://aur.archlinux.org/jsonnet.git' && \
      pushd jsonnet && \
      makepkg -is && \
      popd && popd && \
      rm -rf /tmp/jsonnet && \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh && \
      go install github.com/grafana/tanka/cmd/tk@latest
    ```
    </details>
  </details>

- <details><summary>**MacOS**</summary>
    ```sh
  brew install clang curl git go helm jsonnet kubectl kustomize make && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh && \
    go install github.com/grafana/tanka/cmd/tk@latest
  ```
  </details>

[tanka]: https://tanka.dev
[go-tanka]: https://github.com/grafana/tanka
[jrsonnet]: https://github.com/deltarocks/jrsonnet
[brew]: https://brew.sh
