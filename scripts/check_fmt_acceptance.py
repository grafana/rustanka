#!/usr/bin/env python3
"""Exercise the acceptance gate with actual application binaries and Tanka.

The synthetic 1,000-file corpus reaches the production floor and includes known
upstream second-pass exceptions. Required binaries fail closed; none are skipped.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


class AcceptanceRegression:
    def __init__(self, directory, compare, rtk, tk):
        self.directory = directory
        self.repo = directory / "corpus/repo"
        self.repo.mkdir(parents=True)
        for number in range(1000):
            (self.repo / f"{number:04}.jsonnet").write_bytes(b"1\n")
        cases = {
            0: b"[" * 500 + b"1" + b"]" * 500 + b"\n",
            1: b'"\xff"',
            2: b'"\xc3\xa9\xe2\x82\xac\\uZZZZ"',
            3: b"a['fo\\u006f']\n",
            4: b"{['x']: x for x in [1]}\n",
            5: b"// \xe2\x82\n1\n",
        }
        for number, content in cases.items():
            (self.repo / f"{number:04}.jsonnet").write_bytes(content)
        (self.repo / "vendor").mkdir()
        (self.repo / "0999.jsonnet").rename(self.repo / "vendor/0999.jsonnet")
        (directory / "corpus/repo.rev").write_text("1" * 40 + "\n")
        self.stage = directory / "staged"
        self.stage.mkdir()
        self.sentinel = self.stage / "unrelated"
        self.sentinel.write_bytes(b"preserve this")
        self.extra = directory / "extra"
        self.extra.mkdir()
        self.empty_extra = directory / "empty-extra"
        self.empty_extra.mkdir()
        self.config = directory / "config.toml"
        self.config.write_text('''[[repos]]
name = "repo"
url = "https://example.invalid/acceptance-regression"
ref = "main"
rev = "1111111111111111111111111111111111111111"
[discovery]
exclude = ["**/.*", ".*"]
[baseline]
files = 1000
vendor_files = 1
matching = 1000
matching_formatted = 999
formatted = 999
parse_errors = 1
error_text_matching = 1
already_formatted = 997
non_convergent = 1
refusing_to_reparse = 1
non_utf8 = 2
roots_with_matching_discovery = 1
[known]
output_does_not_reparse = ["repo/0004.jsonnet"]
non_convergent = ["repo/0003.jsonnet"]
''')
        self.arguments = [compare, "fmt-acceptance", "--config", str(self.config),
                          "--corpus-dir", str(directory / "corpus"), "--staged-dir", str(self.stage),
                          "--tk", tk, "--rtk", rtk, "--jobs", "1"]

    def run_case(self, name, expected=0, extra=None, stage=None, diagnostic=None):
        arguments = self.arguments.copy()
        arguments += ["--extra-root", str(extra or self.extra)]
        if stage is not None:
            arguments[arguments.index("--staged-dir") + 1] = str(stage)
        result = subprocess.run(arguments, capture_output=True, timeout=240,
                                env=dict(os.environ, RUST_BACKTRACE="1", RUST_LIB_BACKTRACE="1"))
        log = self.directory / (name + ".log")
        log.write_bytes(result.stdout + result.stderr)
        assert self.sentinel.read_bytes() == b"preserve this", "unrelated staging contents were changed"
        assert result.returncode == expected, f"{name}: exit {result.returncode}, expected {expected}\n{log.read_text(errors='replace')}"
        if diagnostic:
            assert diagnostic in result.stdout + result.stderr, f"{name}: missing diagnostic {diagnostic!r}\n{log.read_text(errors='replace')}"
        print(json.dumps({"case": name, "exit": result.returncode, "sentinel_preserved": True}), flush=True)

    def run(self):
        self.run_case("bytes-depth-exceptions-empty-extra", diagnostic=b"matching = 1000")
        (self.extra / "extra.jsonnet").write_bytes(b"{}\n")
        self.run_case("nonempty-extra", diagnostic=b"extra roots: 1 files, 1 matching")
        self.config.write_text(self.config.read_text().replace("files = 1000", "files = 2000"))
        self.run_case("baseline-with-empty-extra", expected=1, extra=self.empty_extra,
                      diagnostic=b"files is 1000, but the acceptance config records 2000")
        self.run_case("baseline-with-nonempty-extra", expected=1,
                      diagnostic=b"files is 1000, but the acceptance config records 2000")
        self.run_case("source-overlap", expected=1, stage=self.repo, diagnostic=b"overlaps source")
        self.run_case("source-ancestor-overlap", expected=1, stage=self.directory, diagnostic=b"overlaps source")
        if os.name != "nt":
            alias = self.directory / "source-alias"
            alias.symlink_to(self.repo, target_is_directory=True)
            self.run_case("source-symlink-overlap", expected=1, stage=alias / "child", diagnostic=b"overlaps source")


def executable(value):
    found = shutil.which(value)
    if not found:
        raise ValueError(f"required executable not found: {value}")
    return str(Path(found).resolve())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compare", default="target/release/tk-compare")
    parser.add_argument("--rtk", default="target/release/rtk")
    parser.add_argument("--tk", default="tk")
    arguments = parser.parse_args()
    binaries = [executable(value) for value in (arguments.compare, arguments.rtk, arguments.tk)]
    with tempfile.TemporaryDirectory(prefix="rtk-fmt-harness-") as directory:
        AcceptanceRegression(Path(directory), *binaries).run()
    print("Acceptance harness regressions passed.")


if __name__ == "__main__":
    main()
