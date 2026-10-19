"""Run with python3 -m unittest discover -s helper -p 'test_linux.py'."""

import importlib.util
import json
from pathlib import Path
import socket
import sys
import tempfile
import threading
import unittest


spec = importlib.util.spec_from_file_location("scrawl_linux", Path(__file__).with_name("linux.py"))
helper = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = helper
spec.loader.exec_module(helper)


class GeometryTests(unittest.TestCase):
    def test_scaled_window_on_negative_origin_monitor(self):
        geometry = helper.CaptureGeometry.from_status({
            "monitor": {"x": -1920, "y": 0, "width": 1920, "height": 1080},
            "target": {"frame": {"x": -1800, "y": 100, "width": 800,
                                  "height": 600, "scale": 1.5}},
        })
        self.assertEqual(geometry.overlay_bounds(), (120, 100, 800, 600))
        self.assertEqual(geometry.normalize(400, 300), (5000, 5000))
        self.assertEqual(geometry.frame_size(), (1200, 900))
        self.assertEqual(geometry.denormalize(5000, 5000), (400, 300))

    def test_window_crossing_monitor_uses_original_capture_coordinates(self):
        geometry = helper.CaptureGeometry.from_status({
            "monitor": {"x": 1000, "y": 200, "width": 1200, "height": 800},
            "target": {"frame": {"x": 900, "y": 100, "width": 400,
                                  "height": 400, "scale": 1}},
        })
        self.assertEqual(geometry.overlay_bounds(), (0, 0, 300, 300))
        self.assertEqual(geometry.normalize(0, 0), (2500, 2500))
        self.assertIsNone(geometry.normalize(-1, 100))
        self.assertIsNone(geometry.normalize(301, 100))
        self.assertEqual(geometry.normalize(301, 100, clamp=True), (10000, 5000))

    def test_invalid_or_off_monitor_target_does_not_grab_input(self):
        for frame in ({"x": 4000, "y": 0, "width": 20, "height": 20},
                      {"x": 0, "y": 0, "width": 0, "height": 20},
                      {"x": float("nan"), "y": 0, "width": 20, "height": 20}):
            self.assertIsNone(helper.CaptureGeometry.from_status({
                "monitor": {"x": 0, "y": 0, "width": 1920, "height": 1080},
                "target": {"frame": frame},
            }))

    def test_fractional_target_edges_never_emit_invalid_normalized_coordinates(self):
        geometry = helper.CaptureGeometry.from_status({
            "monitor": {"x": 0, "y": 0, "width": 1920, "height": 1080},
            "target": {"frame": {"x": 100.4, "y": 100.4, "width": 400.6,
                                  "height": 400.6, "scale": 1.25}},
        })
        self.assertEqual(geometry.normalize(0, 0), (0, 0))
        self.assertEqual(geometry.normalize(401, 401), (10000, 10000))


class ClockTests(unittest.TestCase):
    def test_recording_clock_advances_and_stops_with_server_state(self):
        clock = helper.RecordingClock()
        clock.update({"state": "recording", "elapsedMs": 3200}, now=10)
        self.assertEqual(clock.elapsed(now=10.25), 3450)
        clock.update({"state": "processing", "elapsedMs": 4000}, now=11)
        self.assertEqual(clock.elapsed(now=99), 4000)

    def test_new_session_clears_ink_even_without_an_idle_poll(self):
        previous = {"state": "recording", "sessionId": "first", "elapsedMs": 300}
        self.assertTrue(helper.new_recording(previous, {
            "state": "recording", "sessionId": "second", "elapsedMs": 400,
        }))
        self.assertFalse(helper.new_recording(previous, {
            "state": "recording", "sessionId": "first", "elapsedMs": 400,
        }))
        self.assertFalse(helper.new_recording(previous, {"state": "idle", "elapsedMs": 0}))


class StatusOrderingTests(unittest.TestCase):
    def test_stroke_acknowledgement_supersedes_all_in_flight_status_polls(self):
        order = helper.StatusOrder()
        stroke = order.issue()
        poll = order.issue()
        self.assertTrue(order.accept(poll))
        # The stroke was queued behind another command; this later-issued poll
        # still saw no ink. The stroke acknowledgement must update the surface.
        self.assertTrue(order.accept(stroke, command=True))
        self.assertFalse(order.accept(poll))
        self.assertTrue(order.accept(order.issue()))


class BundleActionsTests(unittest.TestCase):
    def test_preview_and_copy_use_the_saved_bundle_outside_the_default_directory(self):
        status = {"state": "idle", "bundle": "/tmp/review capture.scrawl"}
        self.assertEqual(helper.bundle_arguments("preview", status),
                         ["preview", "/tmp/review capture.scrawl"])
        self.assertEqual(helper.bundle_arguments("send", status),
                         ["send", "/tmp/review capture.scrawl", "--clipboard"])

    def test_preview_and_copy_fall_back_to_latest_only_without_a_saved_bundle(self):
        for status in ({}, {"bundle": None}, {"bundle": ""}):
            self.assertEqual(helper.bundle_arguments("preview", status), ["preview", "latest"])
            self.assertEqual(helper.bundle_arguments("send", status),
                             ["send", "latest", "--clipboard"])


class DrawingFlushTests(unittest.TestCase):
    def test_external_stop_queues_current_stroke_before_one_acknowledgement(self):
        handshake = helper.DrawingFlush()
        queued = []
        status = {"sessionId": "first", "state": "recording", "flushRequested": True}

        def finish_stroke():
            queued.append({"command": "stroke", "stroke": {"pts": [100, 200, 0]}})

        def send(command):
            queued.append({"command": command})

        handshake.handle(status, finish_stroke, send)
        handshake.handle(status, finish_stroke, send)
        self.assertEqual(queued, [
            {"command": "stroke", "stroke": {"pts": [100, 200, 0]}},
            {"command": "drawing-flushed"},
        ])

    def test_empty_draw_surface_still_acknowledges_and_accepts_the_next_request(self):
        handshake = helper.DrawingFlush()
        queued = []
        status = {"sessionId": "first", "state": "recording", "flushRequested": True}
        handshake.handle(status, lambda: None, queued.append)
        self.assertEqual(queued, ["drawing-flushed"])
        handshake.handle(dict(status, flushRequested=False), lambda: None, queued.append)
        handshake.handle(status, lambda: None, queued.append)
        self.assertEqual(queued, ["drawing-flushed", "drawing-flushed"])


class SocketTests(unittest.TestCase):
    def test_request_reads_fragmented_newline_reply(self):
        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / "control.sock")
            server = socket.socket(socket.AF_UNIX)
            server.bind(path)
            server.listen(1)
            received = []

            def respond():
                with server, server.accept()[0] as connection:
                    received.append(json.loads(connection.makefile("rb").readline()))
                    connection.sendall(b'{"ok":true,"state":')
                    connection.sendall(b'"idle"}\n')

            thread = threading.Thread(target=respond)
            thread.start()
            response = helper.request(path, {"command": "status"})
            thread.join(timeout=1)
            self.assertEqual(received, [{"command": "status"}])
            self.assertEqual(response, {"ok": True, "state": "idle"})


if __name__ == "__main__":
    unittest.main()
