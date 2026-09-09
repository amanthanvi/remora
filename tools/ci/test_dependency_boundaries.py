import contextlib
import io
import subprocess
import unittest
from unittest.mock import patch

import check_dependency_boundaries as guard


ROOTS = {"codex-mobile-client"}
GRAPH = "codex-mobile-client v0.1.0 (/checkout/client)|\nserde v1.0.0|derive\n"


class DependencyBoundaryTests(unittest.TestCase):
    def test_retained_http_fixture_targets_require_explicit_feature(self):
        def metadata(targets):
            return {"packages": [{"name": "codex-rmcp-client", "targets": targets}]}

        for name in ("test_streamable_http_server", "streamable_http_recovery", "streamable_http_remote"):
            with self.subTest(name=name):
                guard.check_fixture_metadata(metadata([{"name": name, "required-features": ["http-test-server"]}]))
                for required in ([], ["unrelated-feature"], "http-test-server", None):
                    with self.assertRaises(ValueError):
                        guard.check_fixture_metadata(metadata([{"name": name, "required-features": required}]))
        guard.check_fixture_metadata(metadata([{"name": "codex_rmcp_client"}]))
        for malformed in ({}, None, {"packages": []}, {"packages": None}, metadata([]), metadata([None])):
            with self.subTest(malformed=malformed):
                with self.assertRaises(ValueError):
                    guard.check_fixture_metadata(malformed)

    def test_safe_graph_and_dependency_removal(self):
        for extra in (
            "",
            "rmcp v1.4.0|client,server,transport-streamable-http-client\nhickory-proto v0.26.2|std\n",
            "hickory-proto v0.26.1|std\nhickory-net v0.26.2|tokio\n",
            "lru v0.18.2|default\nlru v0.18.2|default (*)\n",
            "lru v0.19.0-alpha.1|\nlru v1.0.0+metadata|\n",
            "syn v2.0.0 (proc-macro)|\n",
            "rmcp v1.8.0|client\n",
        ):
            with self.subTest(extra=extra):
                self.assertGreaterEqual(guard.check_graph(GRAPH + extra, ROOTS), 2)

    def test_unsafe_features_versions_and_duplicate_records_fail(self):
        for extra in (
            "rmcp v1.4.0-rc.1|client\n",
            "rmcp v0.15.0|client\n",
            "rmcp v1.8.0|transport-streamable-http-server\n",
            "rmcp v0.15.0|client,transport-streamable-http-server\n",
            "rmcp v0.15.0|server-side-http\n",
            "rmcp v0.15.0|transport-streamable-http-server-session\n",
            "codex-rmcp-client v0.1.0|http-test-server\n",
            "hickory-proto v0.25.2|std,tokio\n",
            "hickory-proto v0.26.0|std\n",
            "hickory-net v0.26.1-rc.1|tokio\n",
            "hickory-net v0.26.0|tokio\n",
            "hickory-net v0.26.2|__dnssec\n",
            "hickory-proto v0.25.2|__dnssec\n",
            "hickory-resolver v0.25.2|dnssec-ring\n",
            "hickory-resolver v0.25.2|dnssec-aws-lc-rs\n",
            "sqlx-mysql v0.8.6|\n",
            "rsa v0.9.10|std\n",
            "rsa v0.10.0-rc.18|std\n",
            "lru v0.12.5|\n",
            "lru v0.18.1|default\n",
            "lru v0.18.2-rc.1|\n",
            "lru v0.18.2|\nlru v0.16.3| (*)\n",
            "rmcp v0.15.0|client\nrmcp v0.15.0|transport-streamable-http-server (*)\n",
        ):
            with self.subTest(extra=extra):
                with self.assertRaises(ValueError):
                    guard.check_graph(GRAPH + extra, ROOTS)

    def test_empty_missing_and_malformed_graphs_fail(self):
        for output in (
            "", "\n", "serde v1.0.0|\n", "codex-mobile-client v0.1.0|\n",
            GRAPH + "warning: incomplete graph\n",
            GRAPH + "lru vbogus|\n",
            GRAPH + "lru v0.18.2\n",
            GRAPH + "lru v0.18.2|default||\n",
            GRAPH + "lru v0.18.2|default,\n",
            GRAPH + "lru v0.18.2|default (unexpected)\n",
        ):
            with self.subTest(output=output):
                with self.assertRaises(ValueError):
                    guard.check_graph(output, ROOTS)
        with self.assertRaises(ValueError):
            guard.check_graph(GRAPH, guard.HOST_ROOTS)

    def test_host_and_mobile_commands_exclude_dev_dependencies_and_lock_resolution(self):
        host_graph = GRAPH + "codex-debug-cli v0.1.0|\ncodex-tui v0.1.0|\n"
        with patch.object(guard.subprocess, "run") as run:
            run.return_value.stdout = host_graph
            guard.check_target(None)
            host = run.call_args.args[0]
            self.assertIn("--workspace", host)
            run.return_value.stdout = GRAPH
            for target in guard.MOBILE_TARGETS:
                guard.check_target(target)
                command = run.call_args.args[0]
                self.assertEqual(command[-4:], ["-p", "codex-mobile-client", "--target", target])
            for call in run.call_args_list:
                command = call.args[0]
                self.assertIn("--locked", command)
                self.assertEqual(command[command.index("--edges") + 1], "normal,build")
                self.assertEqual(command[command.index("--format") + 1], "{p}|{f}")
                self.assertTrue(call.kwargs["check"])

    def test_main_checks_every_target_and_fails_on_command_or_parse_errors(self):
        with patch.object(guard, "check_fixture_targets"), patch.object(guard, "check_target", return_value=2) as check:
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(guard.main(), 0)
            self.assertEqual([call.args[0] for call in check.call_args_list], [None, *guard.MOBILE_TARGETS])
        for error in (
            FileNotFoundError("cargo missing"),
            subprocess.CalledProcessError(1, ["cargo"], stderr="locked graph unavailable"),
            subprocess.TimeoutExpired(["cargo"], 180),
            ValueError("malformed graph"),
        ):
            with self.subTest(error=error):
                with patch.object(guard, "check_fixture_targets"), patch.object(guard, "check_target", side_effect=error):
                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(guard.main(), 1)
                    self.assertIn("failed (host workspace)", stderr.getvalue())
        with patch.object(guard.subprocess, "run") as run:
            run.return_value.stdout = "not json"
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(guard.main(), 1)


if __name__ == "__main__":
    unittest.main()
