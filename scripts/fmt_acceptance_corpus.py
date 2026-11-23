#!/usr/bin/env python3
"""Acquire immutable formatter inputs without replacing existing directories.

Usage: fmt-acceptance-corpus.sh [config] [corpus-dir] [--drift]
Acceptance fetches each configured full commit ID. --drift explicitly fetches
its moving ref into a separate, new corpus and is advisory only.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
try:
    import tomllib
except ImportError:
    raise SystemExit("fmt corpus requires Python 3.11+; set PYTHON to a supported interpreter")
from urllib.parse import unquote, urlparse

OWNER = "rustanka-fmt-corpus-v1"
MARKER = "rtk-fmt-corpus.json"
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")
REV = re.compile(r"[0-9a-fA-F]{40}\Z")


def overlaps(left: Path, right: Path) -> bool:
    return left == right or left in right.parents or right in left.parents


def validate_repos(config: dict) -> list[dict]:
    repos = config.get("repos", [])
    if not repos:
        raise ValueError("config has no [[repos]] entries")
    names = set()
    for repo in repos:
        name = repo.get("name", "")
        if not NAME.fullmatch(name) or name.startswith("extra-"):
            raise ValueError(f"unsafe repository name: {name!r}")
        if name in names:
            raise ValueError(f"duplicate repository name: {name}")
        names.add(name)
        if not REV.fullmatch(repo.get("rev", "")):
            raise ValueError(f"{name} requires an immutable 40-digit rev")
        if not repo.get("url") or not repo.get("ref"):
            raise ValueError(f"{name} requires url and ref")
        if repo["url"].startswith("-") or repo["ref"].startswith("-"):
            raise ValueError(f"{name} url/ref may not be command options")
    return repos


class Corpus:
    def __init__(self, directory: Path, config: Path, drift: bool = False):
        self.directory = directory.resolve()
        self.config = config.resolve(strict=True)
        self.drift = drift
        if self.directory == self.config or self.directory in self.config.parents:
            raise ValueError("corpus directory must not contain its configuration")
        with self.config.open("rb") as stream:
            self.repos = validate_repos(tomllib.load(stream))
        self.mode = "drift" if drift else "acceptance"
        # Validate every destination before creating anything or running Git.
        for repo in self.repos:
            self.validate_destination(repo)

    def validate_destination(self, repo: dict) -> None:
        destination = self.directory / repo["name"]
        if destination.is_symlink():
            raise ValueError(f"repository destination is a symlink: {destination}")
        url = urlparse(repo["url"])
        source = None
        if url.scheme == "file":
            source = Path(unquote(url.path)).resolve()
        elif not url.scheme and Path(repo["url"]).exists():
            source = Path(repo["url"]).resolve()
        if source and overlaps(destination, source):
            raise ValueError(f"repository destination overlaps its source: {destination}")
        revision = self.directory / (repo["name"] + ".rev")
        if revision.is_symlink():
            raise ValueError(f"revision record is a symlink: {revision}")
        if destination.exists():
            marker = destination / ".git" / MARKER
            if not destination.is_dir() or (destination / ".git").is_symlink() or marker.is_symlink():
                raise ValueError(f"unowned repository destination: {destination}")
            try:
                owner = json.loads(marker.read_text())
            except (OSError, ValueError) as error:
                raise ValueError(f"unowned repository destination: {destination}") from error
            if owner.get("owner") != OWNER or owner.get("url") != repo["url"] or owner.get("mode") != self.mode:
                raise ValueError(f"repository ownership/mode mismatch: {destination}")
            expected = repo["ref"] if self.drift else repo["rev"]
            if owner.get("requested") != expected or not owner.get("complete"):
                raise ValueError(f"incomplete or different revision at {destination}; use a new corpus directory")
        elif revision.exists():
            raise ValueError(f"existing revision record has no owned repository: {revision}")

    @staticmethod
    def git(destination: Path, *arguments: str) -> str:
        result = subprocess.run(
            ["git", "-C", os.fspath(destination), *arguments],
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        return result.stdout.decode("utf-8").strip()

    def acquire(self, repo: dict) -> str:
        destination = self.directory / repo["name"]
        requested = repo["ref"] if self.drift else repo["rev"]
        marker = destination / ".git" / MARKER
        if not destination.exists():
            # mkdir is exclusive: even an empty unrelated directory appearing
            # after validation is preserved. Failed fetches remain inspectable.
            destination.mkdir()
            self.git(destination, "init", "--quiet")
            owner = {"owner": OWNER, "url": repo["url"], "mode": self.mode,
                     "requested": requested, "complete": False}
            marker.write_text(json.dumps(owner, sort_keys=True) + "\n")
            self.git(destination, "remote", "add", "origin", repo["url"])
            self.git(destination, "fetch", "--quiet", "--depth=1", "origin", requested)
            actual = self.git(destination, "rev-parse", "FETCH_HEAD")
            if not REV.fullmatch(actual) or (not self.drift and actual.lower() != repo["rev"].lower()):
                raise ValueError(f"{repo['name']}: fetch resolved to unexpected revision {actual!r}")
            self.git(destination, "checkout", "--quiet", "--detach", actual)
            owner.update(complete=True, revision=actual)
            marker.write_text(json.dumps(owner, sort_keys=True) + "\n")
        else:
            self.validate_destination(repo)
            owner = json.loads(marker.read_text())
            actual = self.git(destination, "rev-parse", "HEAD")
            if actual != owner.get("revision") or (not self.drift and actual.lower() != repo["rev"].lower()):
                raise ValueError(f"{repo['name']}: checkout revision changed; use a new corpus directory")
            if self.git(destination, "status", "--porcelain", "--untracked-files=all"):
                raise ValueError(f"{repo['name']}: checkout has modified inputs; use a new corpus directory")
            if self.git(destination, "remote", "get-url", "origin") != repo["url"]:
                raise ValueError(f"{repo['name']}: checkout remote changed")
            if self.drift:
                raise ValueError("drift acquisition requires a new corpus directory for a fresh moving-ref measurement")
        record = self.directory / (repo["name"] + ".rev")
        text = actual + "\n"
        try:
            with record.open("x") as output:
                output.write(text)
        except FileExistsError:
            if record.is_symlink() or record.read_text() != text:
                raise ValueError(f"refusing to overwrite unrelated revision record: {record}")
        print(f"  {repo['name']}: {actual} ({self.mode})", flush=True)
        return actual

    def run(self) -> None:
        self.directory.mkdir(parents=True, exist_ok=True)
        for repo in self.repos:
            self.acquire(repo)
        if self.drift:
            print("Advisory corpus ready; run tk-compare fmt-acceptance --advisory-drift with this corpus.")
        else:
            print("Pinned corpus ready; run make fmt-acceptance with this corpus.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", nargs="?", type=Path, default=Path("fmt-acceptance.toml"))
    parser.add_argument("corpus", nargs="?", type=Path, default=Path("target/fmt-acceptance-corpus"))
    parser.add_argument("--drift", action="store_true", help="fetch moving refs for a separate advisory run")
    args = parser.parse_args()
    try:
        Corpus(args.corpus, args.config, args.drift).run()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"fmt corpus: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError):
            print(error.stderr.decode("utf-8", errors="replace"), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
