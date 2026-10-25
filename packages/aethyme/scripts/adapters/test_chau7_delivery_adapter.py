#!/usr/bin/env python3
"""Offline synthetic protocol/child controls. No live socket, broker or model."""
import importlib.util
import contextlib
import io
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "delivery_adapter", Path(__file__).with_name("chau7-delivery-adapter.py"))
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


class Clock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now


class Socket:
    def __init__(self, chunks, clock=None, advance=0):
        self.chunks = list(chunks)
        self.clock = clock
        self.advance = advance
        self.timeouts = []
        self.closed = False

    def settimeout(self, value):
        self.timeouts.append(value)

    def sendall(self, payload):
        self.sent = payload

    def recv(self, limit):
        if self.clock:
            self.clock.now += self.advance
        chunk = self.chunks.pop(0) if self.chunks else b""
        if len(chunk) > limit:
            self.chunks.insert(0, chunk[limit:])
        return chunk[:limit]

    def connect(self, path):
        self.path = path

    def close(self):
        self.closed = True


def client(sock, budget=None):
    result = object.__new__(adapter.Chau7Client)
    result._sock = sock
    result._buffer = b""
    result._next_id = 0
    result._request_deadline = None
    result._budget = budget or adapter.Deadline()
    return result


class DeliveryBounds(unittest.TestCase):
    def test_interleaved_notification_then_matching_reply(self):
        sock = Socket([b'{"method":"notice"}\n{"id":1,"result":{"ok":true}}\n'])
        result = client(sock)._request("ping", {})
        self.assertEqual(result, {"ok": True})
        self.assertTrue(all(0 < value <= adapter.CALL_SECONDS for value in sock.timeouts))

    def test_trickled_bytes_do_not_restart_request_deadline(self):
        clock = Clock()
        sock = Socket([b"x"] * 20, clock, .4)
        with self.assertRaisesRegex(adapter.Chau7Error, "deadline exhausted"):
            client(sock, adapter.Deadline(1, clock))._request("ping", {})
        self.assertEqual(len(sock.chunks), 17)
        self.assertLess(sock.timeouts[-1], sock.timeouts[0])

    def test_notification_flood_has_a_finite_message_budget(self):
        sock = Socket([b'{"method":"notice"}\n' * (adapter.MAX_INTERLEAVED_MESSAGES + 1)])
        with self.assertRaisesRegex(adapter.Chau7Error, "too many interleaved"):
            client(sock)._request("ping", {})

    def test_oversized_wire_message_is_refused_before_json_decoding(self):
        with patch.object(adapter, "MAX_WIRE_BYTES", 128):
            with self.assertRaisesRegex(adapter.Chau7Error, "message exceeded"):
                client(Socket([b"x" * 129]))._request("ping", {})

    def test_nonobject_json_is_a_typed_protocol_refusal(self):
        with self.assertRaisesRegex(adapter.Chau7Error, "not an object"):
            client(Socket([b"[]\n"]))._request("ping", {})

    def test_initialization_failure_closes_its_socket(self):
        sock = Socket([b"invalid json\n"])
        with patch.object(adapter.socket, "socket", return_value=sock):
            with self.assertRaisesRegex(adapter.Chau7Error, "malformed JSON"):
                adapter.Chau7Client("synthetic-unused-path")
        self.assertTrue(sock.closed)

    def test_expired_pass_cannot_spawn_a_broker_child(self):
        clock = Clock()
        budget = adapter.Deadline(1, clock)
        clock.now = 2
        with patch.object(adapter.subprocess, "Popen") as spawn:
            with self.assertRaisesRegex(adapter.Chau7Error, "deadline exhausted"):
                adapter.run_broker(["must-not-start"], None, budget=budget)
        spawn.assert_not_called()

    def test_broker_timeout_stops_and_reaps_only_its_owned_child(self):
        spawn = subprocess.Popen
        children = []

        def record(*args, **kwargs):
            child = spawn(*args, **kwargs)
            children.append(child)
            return child

        with patch.object(adapter.subprocess, "Popen", side_effect=record):
            with self.assertRaisesRegex(adapter.Chau7Error, "deadline exhausted"):
                adapter.run_broker([sys.executable, "-c", "import time; time.sleep(2)"],
                                   None, budget=adapter.Deadline(.15))
        self.assertEqual(len(children), 1)
        self.assertIsNotNone(children[0].returncode)
        self.assertLess(children[0].returncode, 0, "finite sleeper must be stopped before normal exit")
        self.assertTrue(children[0].stdout.closed and children[0].stderr.closed)

    def test_broker_output_overflow_is_not_treated_as_a_valid_prefix(self):
        with patch.object(adapter, "MAX_WIRE_BYTES", 1024):
            with self.assertRaisesRegex(adapter.Chau7Error, "output exceeded"):
                adapter.run_broker([sys.executable, "-c", "print('x' * 8192)"], None)

    def test_main_shares_one_pass_budget_and_closes_the_client(self):
        fake = unittest.mock.Mock()
        fake.tabs.return_value = []
        claimed = {"claimed": True, "delivery_id": 7, "generation": 2,
                   "action": {"action": "send", "tab_id": "synthetic", "prompt": "fixture"}}
        with patch.object(adapter.sys, "argv", ["adapter", "--worker", "fixture", "--dry-run"]), \
                patch.object(adapter, "Chau7Client", return_value=fake) as factory, \
                patch.object(adapter, "dispatch_once", side_effect=[claimed, {"claimed": False}]) as dispatch, \
                patch.object(adapter, "complete") as complete, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(adapter.main(), 0)
        budget = factory.call_args.kwargs["budget"]
        self.assertIsInstance(budget, adapter.Deadline)
        self.assertTrue(all(call.kwargs.get("budget") is budget for call in dispatch.call_args_list),
                        "all dispatches must share the pass budget")
        self.assertIs(complete.call_args.kwargs["budget"], budget)
        fake.deliver.assert_not_called()
        fake.close.assert_called_once()

    def test_main_closes_client_when_broker_dispatch_fails(self):
        fake = unittest.mock.Mock()
        fake.tabs.return_value = []
        with patch.object(adapter.sys, "argv", ["adapter", "--worker", "fixture"]), \
                patch.object(adapter, "Chau7Client", return_value=fake), \
                patch.object(adapter, "dispatch_once", side_effect=RuntimeError("synthetic refusal")):
            with self.assertRaisesRegex(RuntimeError, "synthetic refusal"):
                adapter.main()
        fake.close.assert_called_once()

    def test_tool_result_shapes_fail_with_typed_protocol_errors(self):
        malformed = [
            ({"isError": True, "content": []}, "returned an error"),
            ({"isError": "false", "content": []}, "non-boolean isError"),
            ({"content": None}, "malformed content"),
            ({"content": {}}, "malformed content"),
            ({"content": ["text"]}, "malformed content"),
            ({"content": [{"type": "text", "text": 7}]}, "non-text text content"),
            ({"content": [{"type": "text", "text": "{}"},
                          {"type": "text", "text": None}]}, "non-text text content"),
        ]
        for result, message in malformed:
            with self.subTest(result=result), patch.object(adapter.Chau7Client, "_request", return_value=result):
                with self.assertRaisesRegex(adapter.Chau7Error, message):
                    client(Socket([])).call_tool("synthetic", {})

    def run_delivery_replies(self, replies):
        real_client = client(Socket([]))
        claimed = {"claimed": True, "delivery_id": 7, "generation": 2,
                   "action": {"action": "send", "tab_id": "synthetic", "prompt": "fixture"}}
        tab_reply = {"content": [{"type": "text", "text": "[]"}]}
        with patch.object(adapter.sys, "argv", ["adapter", "--worker", "fixture"]), \
                patch.object(adapter, "Chau7Client", return_value=real_client), \
                patch.object(real_client, "_request", side_effect=[tab_reply] + replies) as request, \
                patch.object(adapter, "dispatch_once", side_effect=[claimed, {"claimed": False}]), \
                patch.object(adapter, "complete") as complete, \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(adapter.main(), 0)
        self.assertTrue(real_client._sock.closed)
        complete.assert_called_once()
        return [call.args[1]["name"] for call in request.call_args_list], complete.call_args

    def test_failed_send_never_submits_or_acknowledges_delivery(self):
        calls, completion = self.run_delivery_replies([{"isError": True, "content": []}])
        self.assertEqual(calls, ["tab_list", "tab_send_input"])
        self.assertEqual(completion.args[5:], ("retry", "transport_failed"))

    def test_failed_submit_is_not_acknowledged_as_delivered(self):
        calls, completion = self.run_delivery_replies([
            {"content": []}, {"isError": True, "content": []}])
        self.assertEqual(calls, ["tab_list", "tab_send_input", "tab_submit_prompt"])
        self.assertEqual(completion.args[5:], ("retry", "transport_failed"))

    def test_successful_send_and_submit_acknowledge_delivery_once(self):
        calls, completion = self.run_delivery_replies([
            {"isError": False, "content": []}, {"content": []}])
        self.assertEqual(calls, ["tab_list", "tab_send_input", "tab_submit_prompt"])
        self.assertEqual(completion.args[5:], ("delivered",))

    def test_broker_stdin_and_both_output_streams_round_trip(self):
        result = adapter.run_broker(
            [sys.executable, "-c", "import sys; print(len(sys.stdin.read())); print('diagnostic', file=sys.stderr)"],
            None, "x" * 40000)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "40000\n")
        self.assertEqual(result.stderr, "diagnostic\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)
