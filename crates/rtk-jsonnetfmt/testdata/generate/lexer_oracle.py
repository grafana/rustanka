#!/usr/bin/env python3
"""Regenerate the required lexer oracles using pinned go-jsonnet in a temporary copy."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="compare without changing fixtures")
    args = parser.parse_args()
    generator = Path(__file__).resolve().parent
    data = generator.parent
    root = data.parents[2]
    sources = json.loads((data / "lexer-sources.json").read_text())
    if not sources or len(set(sources)) != len(sources):
        raise ValueError("lexer-sources.json must be nonempty and contain unique paths")
    for source in sources:
        path = Path(source)
        if path.is_absolute() or ".." in path.parts or "\n" in source:
            raise ValueError(f"invalid source path: {source!r}")
        if not (root / path).is_file():
            raise FileNotFoundError(root / path)

    env = dict(os.environ, GOFLAGS="-buildvcs=false")
    module = json.loads(subprocess.check_output(
        ["go", "mod", "download", "-json", "github.com/google/go-jsonnet@v0.22.0"],
        cwd=generator, env=env,
    ))
    with tempfile.TemporaryDirectory(prefix="rtk-lexer-oracle-") as temporary:
        stage = Path(temporary)
        upstream = stage / "go-jsonnet"
        shutil.copytree(module["Dir"], upstream)
        # Module-cache files may be read-only. Only the disposable copy is changed.
        for directory, _, files in os.walk(upstream):
            Path(directory).chmod(0o755)
            for name in files:
                (Path(directory) / name).chmod(0o644)
        shutil.copyfile(generator / "fodderdump/fodderdump_test.go",
                        upstream / "internal/parser/rtk_fodderdump_test.go")
        source_list = stage / "sources.txt"
        source_list.write_text("\n".join(sources) + "\n")
        generated = []
        for name, inputs in [
            ("lexer-oracle.json", {"FODDERDUMP_ROOT": str(root),
                                   "FODDERDUMP_SOURCES": str(source_list)}),
            ("lexer-snippet-oracle.json", {"FODDERDUMP_SNIPPETS": str(data / "lexer-snippets.json")}),
            ("string-oracle.json", {"FODDERDUMP_STRINGS": str(data / "string-snippets.json")}),
        ]:
            output = stage / name
            run_env = {key: value for key, value in env.items() if not key.startswith("FODDERDUMP_")}
            run_env.update(inputs, FODDERDUMP_OUT=str(output))
            subprocess.run(["go", "test", "-count=1", "-run", "^TestDump(Fodder|Strings)$", "./internal/parser"],
                           cwd=upstream, env=run_env, check=True)
            generated.append((data / name, output.read_bytes()))
        # Do not replace any saved oracle until all generators have succeeded.
        if args.check:
            stale = [str(path) for path, content in generated if path.read_bytes() != content]
            if stale:
                raise SystemExit("stale lexer oracles; run make update-fmt-lexer-oracle:\n" + "\n".join(stale))
            print("All lexer/string oracles match go-jsonnet v0.22.0 byte for byte")
        else:
            for path, content in generated:
                path.write_bytes(content)


if __name__ == "__main__":
    main()
