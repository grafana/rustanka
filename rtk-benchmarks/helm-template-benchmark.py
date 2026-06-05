#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""
Benchmark rtk's Helm templating cache.

Exports the `helm-template/` fixture (60 inline environments that each render
the same local chart) under three cache regimes and reports the mean wall time
of each via hyperfine:

  1. no-memoization  - RTK_HELM_DISABLE_MEMOIZATION=1, helm runs once per env
  2. in-memory       - default; identical calls collapse to one helm run
  3. warm disk cache - --helm-cache with a pre-populated helm-cache/ directory,
                       so helm runs zero times

Optionally also measures `tk export` for reference (--with-tk).

Requirements: hyperfine, helm, and either a prebuilt rtk binary (--rtk-binary)
or cargo to build one.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURE_DIR = REPO_ROOT / "rtk-benchmarks" / "helm-template"
EXPORT_FORMAT = (
    "{{env.metadata.labels.cluster_name}}/{{.metadata.namespace}}/"
    "{{.kind}}-{{.metadata.name}}"
)


def die(msg: str) -> None:
    print(f"Error: {msg}", file=sys.stderr)
    sys.exit(1)


def require(cmd: str) -> None:
    if shutil.which(cmd) is None:
        die(f"{cmd} is required but not found in PATH")


def resolve_rtk(rtk_binary: str | None) -> Path:
    if rtk_binary:
        path = Path(rtk_binary).resolve()
        if not path.exists():
            die(f"rtk binary does not exist: {path}")
        return path
    print("Building rtk in release mode...", file=sys.stderr)
    subprocess.run(
        ["cargo", "build", "--release", "-p=rtk"],
        cwd=REPO_ROOT,
        check=True,
    )
    return REPO_ROOT / "target" / "release" / "rtk"


def run_hyperfine(
    name: str,
    command: str,
    runs: int,
    warmup: int,
    prepare: str | None,
    json_path: Path,
) -> float:
    """Run one hyperfine benchmark and return the mean time in seconds."""
    args = [
        "hyperfine",
        "--shell",
        "sh",
        "--warmup",
        str(warmup),
        "--runs",
        str(runs),
        "--command-name",
        name,
        "--export-json",
        str(json_path),
    ]
    if prepare:
        args += ["--prepare", prepare]
    args.append(command)
    subprocess.run(args, check=True)
    data = json.loads(json_path.read_text())
    return data["results"][0]["mean"]


def export_cmd(rtk: Path, out_dir: Path, *, helm_cache: bool, replace: bool) -> str:
    parts = [
        str(rtk),
        "export",
        str(out_dir),
        str(FIXTURE_DIR),
        "--recursive",
        "--format",
        f"'{EXPORT_FORMAT}'",
    ]
    if replace:
        parts += ["--merge-strategy", "replace-envs"]
    if helm_cache:
        parts.append("--helm-cache")
    return " ".join(parts)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--rtk-binary",
        help="Path to a prebuilt rtk binary (otherwise built with cargo).",
    )
    parser.add_argument(
        "--runs", type=int, default=10, help="Timed runs per scenario (default: 10)."
    )
    parser.add_argument(
        "--with-tk", action="store_true", help="Also benchmark `tk export`."
    )
    args = parser.parse_args()

    require("hyperfine")
    require("helm")
    if not args.rtk_binary:
        require("cargo")
    if args.with_tk:
        require("tk")

    if not FIXTURE_DIR.exists():
        die(f"fixture directory not found: {FIXTURE_DIR}")

    rtk = resolve_rtk(args.rtk_binary)
    print(f"Using rtk: {rtk}", file=sys.stderr)

    results: dict[str, float] = {}
    with tempfile.TemporaryDirectory(prefix="rtk-helm-bench-") as tmp:
        tmp_path = Path(tmp)
        out_nomemo = tmp_path / "nomemo"
        out_inmem = tmp_path / "inmem"
        out_warm = tmp_path / "warm"
        out_tk = tmp_path / "tk"
        for d in (out_nomemo, out_inmem, out_warm, out_tk):
            d.mkdir()

        json_path = tmp_path / "hf.json"

        # 1. No memoization: helm runs once per environment. Each run is a fresh
        #    process (no cross-run state), so clear only the manifests.
        results["no-memoization"] = run_hyperfine(
            "no-memoization",
            "RTK_HELM_DISABLE_MEMOIZATION=1 "
            + export_cmd(rtk, out_nomemo, helm_cache=False, replace=True),
            args.runs,
            warmup=1,
            prepare=None,
            json_path=json_path,
        )

        # 2. In-memory cache (default): identical calls collapse to one helm run
        #    within each process.
        results["in-memory"] = run_hyperfine(
            "in-memory",
            export_cmd(rtk, out_inmem, helm_cache=False, replace=True),
            args.runs,
            warmup=1,
            prepare=None,
            json_path=json_path,
        )

        # 3. Warm disk cache: --helm-cache. The hyperfine warmup run populates
        #    helm-cache/, so every timed run hits the cache and never invokes helm.
        results["warm-disk-cache"] = run_hyperfine(
            "warm-disk-cache",
            export_cmd(rtk, out_warm, helm_cache=True, replace=True),
            args.runs,
            warmup=1,
            prepare=None,
            json_path=json_path,
        )

        if args.with_tk:
            tk_cmd = " ".join(
                [
                    "tk",
                    "export",
                    str(out_tk),
                    str(FIXTURE_DIR),
                    "--recursive",
                    "--format",
                    f"'{EXPORT_FORMAT}'",
                    "--merge-strategy",
                    "replace-envs",
                ]
            )
            results["tk"] = run_hyperfine(
                "tk",
                tk_cmd,
                args.runs,
                warmup=1,
                prepare=None,
                json_path=json_path,
            )

    print_summary(results)


def print_summary(results: dict[str, float]) -> None:
    print("\n=== Helm templating benchmark (mean wall time) ===")
    baseline = results.get("no-memoization")
    width = max(len(name) for name in results)
    for name, mean in results.items():
        line = f"  {name.ljust(width)}  {mean * 1000:8.1f} ms"
        if baseline and name != "no-memoization":
            line += f"   ({baseline / mean:.2f}x vs no-memoization)"
        print(line)


if __name__ == "__main__":
    main()
