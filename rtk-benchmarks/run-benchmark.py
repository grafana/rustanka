#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyyaml"]
# ///
"""
Benchmark runner that executes benchmarks defined in YAML config files.
"""

import argparse
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

import yaml


@dataclass
class Fixtures:
    static_envs: int
    inline_files: int
    envs_per_inline_file: int
    resources_per_env: int

    @property
    def total_envs(self) -> int:
        return self.static_envs + self.inline_files * self.envs_per_inline_file


@dataclass
class Test:
    name: str
    description: str
    warmup: int
    command: str


@dataclass
class BenchmarkConfig:
    name: str
    id: str
    description: str
    fixtures: Fixtures
    tests: list[Test]

    @classmethod
    def from_yaml(cls, path: Path) -> "BenchmarkConfig":
        with open(path) as f:
            data = yaml.safe_load(f)

        fixtures = Fixtures(**data["fixtures"])
        tests = [Test(**t) for t in data["tests"]]

        return cls(
            name=data["name"],
            id=data["id"],
            description=data["description"],
            fixtures=fixtures,
            tests=tests,
        )


class BenchmarkRunner:
    def __init__(self, config: BenchmarkConfig, repo_root: Path, hyperfine_args: list[str]):
        self.config = config
        self.repo_root = repo_root
        self.hyperfine_args = hyperfine_args
        self.rtk: Path | None = None
        self.rtk_base: Path | None = None
        self.fixtures_dir: Path | None = None

    def check_dependencies(self) -> None:
        """Check that required commands are available."""
        for cmd in ["tk", "hyperfine", "jq", "cargo"]:
            result = subprocess.run(["which", cmd], capture_output=True)
            if result.returncode != 0:
                print(f"Error: {cmd} is required but not found in PATH", file=sys.stderr)
                sys.exit(1)

    def build_rtk(self) -> None:
        """Build rtk in release mode."""
        print("Building rtk in release mode...", file=sys.stderr)
        subprocess.run(
            ["cargo", "build", "--release", "-p", "rtk"],
            cwd=self.repo_root,
            check=True,
        )
        self.rtk = self.repo_root / "target" / "release" / "rtk"

    def build_rtk_base(self) -> None:
        """Build rtk from base branch if BENCHMARK_BASE_REF is set."""
        base_ref = os.environ.get("BENCHMARK_BASE_REF", "")
        if not base_ref:
            self.rtk_base = None
            return

        print(f"Building rtk from base branch ({base_ref})...", file=sys.stderr)

        # Save current HEAD
        current_head = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=self.repo_root,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()

        try:
            # Checkout base branch
            subprocess.run(
                ["git", "checkout", "--quiet", f"origin/{base_ref}"],
                cwd=self.repo_root,
                check=True,
            )

            # Build to separate target directory
            env = os.environ.copy()
            env["CARGO_TARGET_DIR"] = str(self.repo_root / "target-base")
            subprocess.run(
                ["cargo", "build", "--release", "-p", "rtk"],
                cwd=self.repo_root,
                env=env,
                check=True,
            )

            self.rtk_base = self.repo_root / "target-base" / "release" / "rtk"
        finally:
            # Restore current HEAD
            subprocess.run(
                ["git", "checkout", "--quiet", current_head],
                cwd=self.repo_root,
                check=True,
            )

        version = subprocess.run(
            [str(self.rtk_base), "--version"],
            capture_output=True,
            text=True,
        ).stdout.strip()
        print(f"Built rtk-base: {version}", file=sys.stderr)

    def generate_fixtures(self, fixtures_dir: Path) -> None:
        """Generate test fixtures."""
        self.fixtures_dir = fixtures_dir

        # Source the bash library and call generate_fixtures
        script = f"""
        set -euo pipefail
        NUM_STATIC_ENVS={self.config.fixtures.static_envs}
        NUM_INLINE_FILES={self.config.fixtures.inline_files}
        ENVS_PER_INLINE_FILE={self.config.fixtures.envs_per_inline_file}
        NUM_RESOURCES_PER_ENV={self.config.fixtures.resources_per_env}
        source "{self.repo_root}/rtk-benchmarks/lib/generate-fixtures.sh"
        generate_fixtures "{fixtures_dir}"
        """
        subprocess.run(["bash", "-c", script], check=True)

    def get_path_vars(self) -> dict[str, str]:
        """Get path variables for command substitution."""
        assert self.fixtures_dir is not None
        return {
            "fixtures_dir": str(self.fixtures_dir),
            "single_static_dir": str(self.fixtures_dir / "static-0001"),
            "single_inline_dir": str(self.fixtures_dir / "inline-01"),
            "single_inline_file": str(self.fixtures_dir / "inline-01" / "main.jsonnet"),
            "static_envs": str(self.config.fixtures.static_envs),
            "inline_files": str(self.config.fixtures.inline_files),
            "envs_per_inline_file": str(self.config.fixtures.envs_per_inline_file),
            "resources_per_env": str(self.config.fixtures.resources_per_env),
            "total_envs": str(self.config.fixtures.total_envs),
        }

    def expand_command(self, command: str) -> str:
        """Expand placeholders in command."""
        result = command
        for key, value in self.get_path_vars().items():
            result = result.replace(f"{{{key}}}", value)
        return result

    def run_command(self, binary: str, command: str) -> subprocess.CompletedProcess:
        """Run a command with the given binary."""
        full_cmd = f"{binary} {command}"
        return subprocess.run(
            ["sh", "-c", full_cmd],
            capture_output=True,
            text=True,
        )

    def validate_test(self, test: Test) -> None:
        """Validate that tk and rtk produce matching output."""
        command = self.expand_command(test.command)
        print(f"Validating {test.name}... ", end="", file=sys.stderr, flush=True)

        tk_result = self.run_command("tk", command)
        rtk_result = self.run_command(str(self.rtk), command)

        if tk_result.returncode != 0:
            print(f"ERROR: tk failed with exit code {tk_result.returncode}", file=sys.stderr)
            print(f"stderr: {tk_result.stderr}", file=sys.stderr)
            self._fail_validation(f"tk command failed: {command}")

        if rtk_result.returncode != 0:
            print(f"ERROR: rtk failed with exit code {rtk_result.returncode}", file=sys.stderr)
            print(f"stderr: {rtk_result.stderr}", file=sys.stderr)
            self._fail_validation(f"rtk command failed: {command}")

        # For JSON output, compare parsed JSON for equality (order-independent)
        # Otherwise compare byte-for-byte
        if "--json" in test.command or test.command.startswith("eval "):
            if not self._json_equal(tk_result.stdout, rtk_result.stdout):
                print("JSON MISMATCH!", file=sys.stderr)
                self._show_diff("tk", "rtk", tk_result.stdout, rtk_result.stdout)
                self._fail_validation(f"rtk JSON output differs from tk for: {command}")
        else:
            if tk_result.stdout != rtk_result.stdout:
                print("OUTPUT MISMATCH!", file=sys.stderr)
                self._show_diff("tk", "rtk", tk_result.stdout, rtk_result.stdout)
                self._fail_validation(f"rtk output differs from tk for: {command}")

        print("OK", file=sys.stderr, flush=True)

    def _json_equal(self, json1: str, json2: str) -> bool:
        """Compare two JSON strings for equality (ignoring key order)."""
        import json
        try:
            return json.loads(json1) == json.loads(json2)
        except json.JSONDecodeError:
            # If not valid JSON, fall back to string comparison
            return json1 == json2

    def _show_diff(self, name1: str, name2: str, output1: str, output2: str) -> None:
        """Show a summary of differences between two outputs."""
        lines1 = output1.splitlines()
        lines2 = output2.splitlines()

        print(f"\n--- {name1} ({len(lines1)} lines, {len(output1)} bytes)", file=sys.stderr)
        print(f"+++ {name2} ({len(lines2)} lines, {len(output2)} bytes)", file=sys.stderr)

        # Show first difference
        for i, (l1, l2) in enumerate(zip(lines1, lines2)):
            if l1 != l2:
                print(f"\nFirst difference at line {i + 1}:", file=sys.stderr)
                print(f"  {name1}: {l1[:200]!r}", file=sys.stderr)
                print(f"  {name2}: {l2[:200]!r}", file=sys.stderr)
                break
        else:
            if len(lines1) != len(lines2):
                print(f"\nLine count differs: {len(lines1)} vs {len(lines2)}", file=sys.stderr)

        sys.stderr.flush()

    def _fail_validation(self, message: str) -> None:
        """Print validation failure and exit."""
        print(f"\n## Validation Failed\n\n{message}\n", flush=True)
        sys.stdout.flush()
        sys.stderr.flush()
        sys.exit(1)

    def run_benchmark(self, test: Test, output_file: Path, index: int) -> None:
        """Run hyperfine benchmark for a test."""
        command = self.expand_command(test.command)
        description = self.expand_command(test.description)

        print(f"### {test.name}")
        print()
        print(description)
        print()

        # Build hyperfine command
        temp_md = output_file.with_suffix(f".{index}")
        args = [
            "hyperfine",
            "-N",
            "--warmup", str(test.warmup),
            *self.hyperfine_args,
            "--export-markdown", str(temp_md),
            "-n", "tk", f"sh -c 'tk {command} >/dev/null'",
            "-n", "rtk", f"sh -c '{self.rtk} {command} >/dev/null'",
        ]

        if self.rtk_base:
            args.extend(["-n", "rtk-base", f"sh -c '{self.rtk_base} {command} >/dev/null'"])

        subprocess.run(args, check=True)

        # Append markdown table to output
        with open(temp_md) as f:
            print(f.read())
        print()

    def print_header(self) -> None:
        """Print benchmark header."""
        print(f"# RTK vs Tanka {self.config.name} Benchmarks")
        print()
        print(self.config.description)
        print()
        print("## Test Configuration")
        print()
        print(f"- Static environments: {self.config.fixtures.static_envs}")
        print(f"- Inline environment files: {self.config.fixtures.inline_files} "
              f"({self.config.fixtures.envs_per_inline_file} envs each = "
              f"{self.config.fixtures.inline_files * self.config.fixtures.envs_per_inline_file} total)")
        print(f"- Resources per environment: {self.config.fixtures.resources_per_env}")
        print(f"- Total environments: {self.config.fixtures.total_envs}")
        print()

    def print_versions(self) -> None:
        """Print version information."""
        # tk outputs version to stderr
        tk_result = subprocess.run(
            ["tk", "--version"],
            capture_output=True,
            text=True,
        )
        tk_version = (tk_result.stdout or tk_result.stderr).strip()
        rtk_version = subprocess.run(
            [str(self.rtk), "--version"],
            capture_output=True,
            text=True,
        ).stdout.strip()

        print("## Versions")
        print()
        print(f"- tk: {tk_version}")
        print(f"- rtk: {rtk_version}")
        if self.rtk_base:
            rtk_base_version = subprocess.run(
                [str(self.rtk_base), "--version"],
                capture_output=True,
                text=True,
            ).stdout.strip()
            print(f"- rtk-base: {rtk_base_version}")
        print()

    def run(self) -> None:
        """Run the benchmark."""
        self.check_dependencies()
        self.build_rtk()
        self.build_rtk_base()

        self.print_header()
        self.print_versions()

        output_file = Path(os.environ.get("BENCHMARK_MARKDOWN_OUTPUT", tempfile.mktemp()))

        with tempfile.TemporaryDirectory() as tmpdir:
            self.generate_fixtures(Path(tmpdir))

            print("Validating outputs match before benchmarking...", file=sys.stderr)
            for test in self.config.tests:
                self.validate_test(test)
            print(file=sys.stderr)

            print("## Benchmarks")
            print()

            # Capture markdown output
            markdown_lines = []
            for i, test in enumerate(self.config.tests, 1):
                self.run_benchmark(test, output_file, i)

        print(f"Markdown output written to: {output_file}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(
        description="Run benchmarks from YAML config",
        usage="%(prog)s config [-- hyperfine_args...]",
    )
    parser.add_argument("config", type=Path, help="Path to benchmark YAML config file")
    parser.add_argument("hyperfine_args", nargs=argparse.REMAINDER, help="Additional arguments to pass to hyperfine (after --)")
    args = parser.parse_args()

    # Remove leading '--' if present
    hyperfine_args = args.hyperfine_args
    if hyperfine_args and hyperfine_args[0] == "--":
        hyperfine_args = hyperfine_args[1:]

    repo_root = Path(__file__).parent.parent.resolve()
    config = BenchmarkConfig.from_yaml(args.config)
    runner = BenchmarkRunner(config, repo_root, hyperfine_args)
    runner.run()


if __name__ == "__main__":
    main()
