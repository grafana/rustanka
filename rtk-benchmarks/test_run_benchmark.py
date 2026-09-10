"""Regression coverage for benchmark comparisons without Tanka."""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "run_benchmark", Path(__file__).with_name("run-benchmark.py"))
benchmark = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = benchmark
spec.loader.exec_module(benchmark)


class BaseOnlyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        config = benchmark.BenchmarkConfig(
            name="test", id="test", description="", tests=[],
            mode="static", skip_tk=True)
        self.runner = benchmark.BenchmarkRunner(config, self.root, [],
                                                rtk_path=self.root / "rtk")
        self.runner.rtk = self.root / "rtk"
        self.runner.rtk_base = self.root / "rtk-base"
        self.runner.fixtures_dir = self.root
        self.runner.export_dir_rtk = self.root / "current"
        self.runner.export_dir_rtk_base = self.root / "base"
        self.runner.export_dir_rtk.mkdir()
        self.runner.export_dir_rtk_base.mkdir()

    def result(self, stdout="", code=0):
        return subprocess.CompletedProcess([], code, stdout, "failure" if code else "")

    def test_no_tanka_dependency(self):
        with patch.object(benchmark.subprocess, "run", return_value=self.result()) as run:
            self.runner.check_dependencies()
        self.assertNotIn(unittest.mock.call(["which", "tk"], capture_output=True), run.call_args_list)

    def test_json_compared_against_base_in_generated_and_static_modes(self):
        for mode in ("generated", "static"):
            with self.subTest(mode=mode):
                self.runner.config.mode = mode
                with patch.object(self.runner, "expand_command", side_effect=lambda command, _: command), \
                        patch.object(self.runner, "run_command", side_effect=[
                            self.result('{"a":1,"b":2}'), self.result('{"b":2,"a":1}')]) as run:
                    self.runner.validate_test(benchmark.Test("json", command="eval ."))
                self.assertEqual([call.args[0] for call in run.call_args_list],
                                 [str(self.runner.rtk), str(self.runner.rtk_base)])

    def test_base_failure_is_not_silently_skipped(self):
        with patch.object(self.runner, "run_command", side_effect=[
                self.result("same"), self.result(code=1)]), self.assertRaises(SystemExit):
            self.runner.validate_test(benchmark.Test("failed", command="show ."))

    def test_export_file_names_and_exact_bytes_are_compared(self):
        test = benchmark.Test("export", command="export {export_dir} .")
        current = self.runner.export_dir_rtk / "resource.yaml"
        base = self.runner.export_dir_rtk_base / "resource.yaml"
        current.write_bytes(b"value: 1\n")
        base.write_bytes(b"value: 1\n")
        with patch.object(self.runner, "run_command", return_value=self.result()):
            self.runner.validate_test(test)
            for content in (b"value: 2\n", b"value: 1\nextra", b""):
                base.write_bytes(content)
                with self.subTest(content=content), self.assertRaises(SystemExit):
                    self.runner.validate_test(test)
            base.unlink()
            with self.assertRaises(SystemExit):
                self.runner.validate_test(test)

    def test_prepare_runs_for_both_binaries(self):
        self.runner.config.prepare = "prepare {export_dir}"
        with patch.object(self.runner, "run_command", return_value=self.result()), \
                patch.object(benchmark.subprocess, "run") as run:
            self.runner.validate_test(benchmark.Test("eval", command="eval ."))
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ["sh", "-c", f"prepare {self.runner.export_dir_rtk}"],
            ["sh", "-c", f"prepare {self.runner.export_dir_rtk_base}"]])

    def test_diff_omits_tanka_and_rejects_base_failure(self):
        self.runner.config.mode = "diff"
        self.runner.config.fixtures_dir = str(self.root)
        (self.root / "diff-case" / "cluster").mkdir(parents=True)
        for base_code in (0, 1):
            calls = []

            def execute(args, **kwargs):
                calls.append(args)
                if args[0] == "hyperfine":
                    Path(args[args.index("--export-markdown") + 1]).write_text("benchmark")
                return self.result("same", base_code if "rtk-base" in args[-1] else 0)

            with self.subTest(base_code=base_code), \
                    patch.object(self.runner, "start_mock_server"), \
                    patch.object(self.runner, "stop_mock_server") as stop, \
                    patch.object(self.runner, "_parse_benchmark_json", return_value={}), \
                    patch.object(benchmark.subprocess, "run", side_effect=execute):
                if base_code:
                    with self.assertRaises(SystemExit):
                        self.runner.run_diff_benchmark(benchmark.Test("diff-case"), self.root, self.root / "out.md", 1)
                    self.assertFalse(any(call[0] == "hyperfine" for call in calls))
                else:
                    self.runner.run_diff_benchmark(benchmark.Test("diff-case"), self.root, self.root / "out.md", 1)
                    hyperfine = next(call for call in calls if call[0] == "hyperfine")
                    self.assertNotIn("tk diff", hyperfine)
                    self.assertIn("rtk-base diff", hyperfine)
                stop.assert_called_once()

    def test_generated_timing_has_only_rtk_and_base(self):
        calls = []

        def execute(args, **kwargs):
            calls.append(args)
            Path(args[args.index("--export-markdown") + 1]).write_text("benchmark")
            return self.result()

        with patch.object(benchmark.subprocess, "run", side_effect=execute), \
                patch.object(self.runner, "_parse_benchmark_json", return_value={}), \
                patch.object(self.runner, "_check_rtk_base_supports_command") as preflight:
            self.runner.run_generated_benchmark(
                benchmark.Test("export", command="export {export_dir} ."), self.root / "out.md", 1)
        preflight.assert_not_called()
        names = [calls[0][i + 1] for i, arg in enumerate(calls[0]) if arg == "-n"]
        self.assertEqual(names, ["rtk", "rtk-base"])

    def test_output_mismatch_fails(self):
        for command in ("show .", "eval .", "env list --json ."):
            with self.subTest(command=command), \
                    patch.object(self.runner, "run_command", side_effect=[
                        self.result('{"a":1}'), self.result('{"a":2}')]), \
                    self.assertRaises(SystemExit):
                self.runner.validate_test(benchmark.Test("mismatch", command=command))

    def test_default_still_validates_against_tanka(self):
        self.runner.config.skip_tk = False
        with patch.object(self.runner, "run_command", return_value=self.result()) as run:
            self.runner.validate_test(benchmark.Test("show", command="show ."))
        self.assertEqual([call.args[0] for call in run.call_args_list],
                         [str(self.runner.rtk), "tk"])

    def test_cli_enables_skip_tk(self):
        config = self.root / "bench.yaml"
        config.write_text("name: Test\nid: test\ndescription: test\nfixtures_dir: fixtures\ntests:\n  - name: sample\n")
        with patch.object(sys, "argv", ["run-benchmark.py", str(config), "--skip-tk"]), \
                patch.object(benchmark, "BenchmarkRunner") as runner:
            benchmark.main()
        self.assertTrue(runner.call_args.args[0].skip_tk)
        self.assertEqual(runner.call_args.args[2], [])


if __name__ == "__main__":
    unittest.main()
