#!/usr/bin/env python3
"""Generate mandatory AST/pass answers with pinned Go, or check small regressions."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


class OracleGenerator:
    def __init__(self, kind):
        self.kind = kind
        self.directory = Path(__file__).resolve().parent
        self.data = self.directory.parent
        self.root = self.data.parents[2]
        self.env = dict(os.environ, GOFLAGS="-buildvcs=false")

    @staticmethod
    def read_cases(path, key):
        cases = json.loads(path.read_bytes())
        if not isinstance(cases, list) or not cases:
            raise ValueError(f"{path}: expected a nonempty array")
        names = [case[key] for case in cases]
        if any(not name for name in names) or len(set(names)) != len(names):
            raise ValueError(f"{path}: empty or duplicate {key}")
        return cases

    def upstream(self):
        module = json.loads(subprocess.check_output(
            ["go", "mod", "download", "-json", "github.com/google/go-jsonnet@v0.22.0"],
            cwd=self.directory, env=self.env,
        ))
        source = Path(module["Dir"])
        if module.get("Version") != "v0.22.0":
            raise ValueError("Go did not resolve go-jsonnet v0.22.0")
        for relative in ["go.mod", "internal/formatter/jsonnetfmt.go",
                         "formatter/testdata/empty_comment.jsonnet"]:
            if not (source / relative).is_file():
                raise FileNotFoundError(source / relative)
        return source

    def copy_upstream(self, destination):
        shutil.copytree(self.upstream(), destination)
        for directory, _, files in os.walk(destination):
            Path(directory).chmod(0o755)
            for name in files:
                (Path(directory) / name).chmod(0o644)

    def prepare_checkout(self, destination):
        explicit = os.environ.get("GO_JSONNET_FOR_TESTS")
        if explicit:
            source = Path(explicit)
            # Compare the actual pinned module files, not a tag-like directory name.
            pinned = self.upstream()
            for expected in pinned.rglob("*"):
                if not expected.is_file():
                    continue
                relative = expected.relative_to(pinned)
                if (source / relative).read_bytes() != expected.read_bytes():
                    raise ValueError(f"{source / relative}: differs from pinned go-jsonnet v0.22.0")
            return
        pinned = self.upstream()
        if destination.is_symlink():
            raise ValueError(f"refusing symlink checkout: {destination}")
        if destination.exists():
            # Repair an incomplete build-cache checkout additively. Never delete
            # a caller's tree or overwrite files whose ownership is unknown.
            for source in pinned.rglob("*"):
                target = destination / source.relative_to(pinned)
                if target.is_symlink():
                    raise ValueError(f"refusing symlink in checkout: {target}")
                if source.is_dir():
                    target.mkdir(parents=True, exist_ok=True)
                elif target.exists():
                    if target.read_bytes() != source.read_bytes():
                        raise ValueError(f"checkout differs from pinned module: {target}")
                else:
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(source, target)
        else:
            destination.parent.mkdir(parents=True, exist_ok=True)
            self.copy_upstream(destination)

    def generate(self, output, corpus=False, all_snippets=False):
        with tempfile.TemporaryDirectory(prefix="rtk-ast-oracle-") as temporary:
            stage = Path(temporary)
            cwd, command = self.directory, ["go", "run", "./nodedump"]
            if corpus:
                manifest_path = self.data / "corpus/manifest.json"
                manifest = json.loads(manifest_path.read_bytes())
                cases = manifest["entries"]
                if not cases or len({case["source"] for case in cases}) != len(cases):
                    raise ValueError(f"{manifest_path}: empty or duplicate sources")
                for case in cases:
                    path = Path(case["source"])
                    if path.is_absolute() or ".." in path.parts or not (self.root / path).is_file():
                        raise ValueError(f"{manifest_path}: invalid or missing source {path}")
                arguments = ["files", str(self.root), str(manifest_path)]
            else:
                suffix = "snippets" if all_snippets else "regressions"
                inputs = self.data / f"{self.kind}-{suffix}.json"
                cases = self.read_cases(inputs, "name")
                arguments = ["snippets", str(inputs)]
            generated = stage / "answer.json"
            subprocess.run(command + arguments + [str(generated)], cwd=cwd, env=self.env, check=True)
            answers = self.read_cases(generated, "source")
            expected = [case["source" if corpus else "name"] for case in cases]
            if [answer["source"] for answer in answers] != expected:
                raise ValueError("Go output does not cover every required input in order")
            for answer in answers:
                if not answer.get("entries") and not answer.get("error"):
                    raise ValueError(f"Go output contains no AST or refusal: {answer['source']}")
            output.write_bytes(generated.read_bytes())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=["node"], default="node")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--corpus-output", type=Path)
    mode.add_argument("--snippets-output", type=Path)
    mode.add_argument("--checkout", type=Path)
    args = parser.parse_args()
    generator = OracleGenerator(args.kind)
    if args.checkout:
        generator.prepare_checkout(args.checkout)
    elif args.corpus_output or args.snippets_output:
        generator.generate(args.corpus_output or args.snippets_output,
                           corpus=bool(args.corpus_output), all_snippets=bool(args.snippets_output))
    else:
        name = f"{args.kind}-regression-oracle.json"
        saved = generator.data / name
        with tempfile.TemporaryDirectory(prefix="rtk-ast-check-") as temporary:
            output = Path(temporary) / name
            generator.generate(output)
            if args.check:
                if saved.read_bytes() != output.read_bytes():
                    raise SystemExit(f"stale oracle: {saved}; regenerate with ast_oracle.py --kind {args.kind}")
                print(f"{name} matches go-jsonnet v0.22.0 byte for byte")
            else:
                saved.write_bytes(output.read_bytes())


if __name__ == "__main__":
    main()
