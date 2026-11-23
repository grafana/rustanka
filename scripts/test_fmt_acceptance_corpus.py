"""Safety and revision-contract tests; Git is replaced by an in-memory fake."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from fmt_acceptance_corpus import Corpus, MARKER, OWNER

PIN = "1" * 40
DRIFT = "2" * 40


class CorpusTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rtk-corpus-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.config = self.root / "config.toml"
        self.directory = self.root / "corpus"
        self.write_config()
        self.commands = []

    def write_config(self, name="example", rev=PIN, url="https://example.test/repo", suffix=""):
        self.config.write_text(f'[[repos]]\nname = {json.dumps(name)}\nurl = {json.dumps(url)}\nref = "main"\nrev = {json.dumps(rev)}\n' + suffix)

    def fake_git(self, destination, *arguments):
        self.commands.append(arguments)
        if arguments[0] == "init":
            (destination / ".git").mkdir()
        if arguments[:1] == ("rev-parse",):
            return PIN
        if arguments == ("remote", "get-url", "origin"):
            return "https://example.test/repo"
        return ""

    def test_unrelated_directory_and_sentinel_survive(self):
        destination = self.directory / "example"
        destination.mkdir(parents=True)
        sentinel = destination / "sentinel"
        sentinel.write_bytes(b"keep me")
        with self.assertRaisesRegex(ValueError, "unowned"):
            Corpus(self.directory, self.config)
        self.assertEqual(sentinel.read_bytes(), b"keep me")
        self.assertEqual(self.commands, [])

    def test_unrelated_parent_contents_survive_acquisition(self):
        self.directory.mkdir()
        sentinel = self.directory / "sentinel"
        sentinel.write_bytes(b"keep me")
        with patch.object(Corpus, "git", side_effect=self.fake_git):
            Corpus(self.directory, self.config).run()
        self.assertEqual(sentinel.read_bytes(), b"keep me")

    def test_names_cannot_escape_or_alias(self):
        for name in ("", ".", "..", "../escape", "/absolute", "a/b", "a\\b", "extra-1", "-options"):
            with self.subTest(name=name):
                self.write_config(name=name)
                with self.assertRaisesRegex(ValueError, "unsafe"):
                    Corpus(self.directory, self.config)
        self.assertFalse(self.directory.exists())

    def test_all_destinations_are_validated_before_creating_any(self):
        self.write_config(suffix=f'[[repos]]\nname = "second"\nurl = "https://example.test/repo"\nref = "main"\nrev = "{PIN}"\n')
        (self.directory / "second").mkdir(parents=True)
        with self.assertRaisesRegex(ValueError, "unowned"):
            Corpus(self.directory, self.config)
        self.assertFalse((self.directory / "example").exists())

    def test_duplicate_names_and_missing_pins_fail(self):
        text = self.config.read_text()
        self.config.write_text(text + text)
        with self.assertRaisesRegex(ValueError, "duplicate"):
            Corpus(self.directory, self.config)
        self.write_config(rev="main")
        with self.assertRaisesRegex(ValueError, "immutable"):
            Corpus(self.directory, self.config)

    def test_source_overlap_and_symlink_aliases_fail(self):
        self.directory.mkdir()
        source = self.root / "source"
        source.mkdir()
        self.write_config(url=source.as_uri())
        (self.directory / "example").symlink_to(source, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "symlink"):
            Corpus(self.directory, self.config)
        self.write_config(url=self.directory.as_uri())
        with self.assertRaises(ValueError):
            Corpus(self.directory, self.config)
        with self.assertRaisesRegex(ValueError, "configuration"):
            Corpus(self.root, self.config)

    def test_local_source_ancestor_descendant_and_alias_overlap(self):
        self.directory.mkdir()
        alias = self.root / "source-alias"
        alias.symlink_to(self.directory, target_is_directory=True)
        for source in (self.directory, self.directory / "example/source", alias):
            with self.subTest(source=source):
                self.write_config(url=source.as_uri())
                with self.assertRaisesRegex(ValueError, "overlaps its source"):
                    Corpus(self.directory, self.config)
        self.assertFalse((self.directory / "example").exists())

    def test_new_unrelated_destination_between_validation_and_acquisition_survives(self):
        corpus = Corpus(self.directory, self.config)
        destination = self.directory / "example"
        destination.mkdir(parents=True)
        sentinel = destination / "sentinel"
        sentinel.write_text("keep")
        with self.assertRaisesRegex(ValueError, "unowned"):
            corpus.run()
        self.assertEqual(sentinel.read_text(), "keep")

    def test_revision_symlink_is_not_overwritten(self):
        self.directory.mkdir()
        sentinel = self.root / "sentinel"
        sentinel.write_bytes(b"keep")
        (self.directory / "example.rev").symlink_to(sentinel)
        with self.assertRaisesRegex(ValueError, "symlink"):
            Corpus(self.directory, self.config)
        self.assertEqual(sentinel.read_bytes(), b"keep")

    def test_acceptance_fetches_exact_pin_and_reuses_only_verified_owned_clone(self):
        with patch.object(Corpus, "git", side_effect=self.fake_git):
            Corpus(self.directory, self.config).run()
            Corpus(self.directory, self.config).run()
        self.assertIn(("fetch", "--quiet", "--depth=1", "origin", PIN), self.commands)
        self.assertNotIn(("fetch", "--quiet", "--depth=1", "origin", "main"), self.commands)
        self.assertEqual((self.directory / "example.rev").read_text(), PIN + "\n")
        self.assertEqual(sum(c[0] == "fetch" for c in self.commands), 1)

    def test_drift_uses_ref_and_never_becomes_acceptance(self):
        def drift_git(destination, *args):
            result = self.fake_git(destination, *args)
            return DRIFT if args[0] == "rev-parse" else result
        with patch.object(Corpus, "git", side_effect=drift_git):
            Corpus(self.directory, self.config, drift=True).run()
        self.assertIn(("fetch", "--quiet", "--depth=1", "origin", "main"), self.commands)
        self.assertEqual((self.directory / "example.rev").read_text(), DRIFT + "\n")
        with self.assertRaisesRegex(ValueError, "mode mismatch"):
            Corpus(self.directory, self.config)

    def test_unavailable_revision_keeps_inspectable_owned_directory(self):
        def failing_git(destination, *args):
            if args[0] == "fetch":
                raise OSError("revision unavailable")
            return self.fake_git(destination, *args)
        with patch.object(Corpus, "git", side_effect=failing_git):
            with self.assertRaisesRegex(OSError, "unavailable"):
                Corpus(self.directory, self.config).run()
        destination = self.directory / "example"
        self.assertTrue(destination.is_dir())
        self.assertFalse(json.loads((destination / ".git" / MARKER).read_text())["complete"])
        self.assertFalse((self.directory / "example.rev").exists())
        with self.assertRaisesRegex(ValueError, "incomplete"):
            Corpus(self.directory, self.config)

    def test_existing_revision_and_ownership_mismatch_are_not_overwritten(self):
        with patch.object(Corpus, "git", side_effect=self.fake_git):
            Corpus(self.directory, self.config).run()
            record = self.directory / "example.rev"
            record.write_text("unrelated\n")
            with self.assertRaisesRegex(ValueError, "overwrite"):
                Corpus(self.directory, self.config).run()
            self.assertEqual(record.read_text(), "unrelated\n")
        marker = self.directory / "example" / ".git" / MARKER
        marker.write_text(json.dumps({"owner": OWNER, "url": "elsewhere", "mode": "acceptance"}))
        with self.assertRaisesRegex(ValueError, "ownership"):
            Corpus(self.directory, self.config)


if __name__ == "__main__":
    unittest.main()
