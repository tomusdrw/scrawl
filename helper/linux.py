#!/usr/bin/env python3
"""Native Linux controls. Rust owns capture state; this process only draws UI.

Embedded by the Rust executable and launched with ``python3 -c SOURCE``.
All IPC and child processes run outside the GTK main thread. Importing this
module does not initialize GTK, so geometry and the protocol can be tested
without a compositor.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
import hashlib
import json
import math
import socket
import subprocess
import sys
import time


@dataclass(frozen=True)
class CaptureGeometry:
    x: float
    y: float
    width: float
    height: float
    scale: float
    monitor_x: float
    monitor_y: float
    monitor_width: float
    monitor_height: float

    @classmethod
    def from_status(cls, status):
        frame = (status.get("target") or {}).get("frame")
        monitor = status.get("monitor")
        if not frame or not monitor:
            return None
        try:
            geometry = cls(*(float(frame[key]) for key in ("x", "y", "width", "height")),
                           float(frame.get("scale", 1)),
                           *(float(monitor[key]) for key in ("x", "y", "width", "height")))
        except (KeyError, TypeError, ValueError):
            return None
        if not all(math.isfinite(value) for value in vars(geometry).values()):
            return None
        if min(geometry.width, geometry.height, geometry.scale,
               geometry.monitor_width, geometry.monitor_height) <= 0:
            return None
        if min(geometry.overlay_bounds()[2:]) <= 0:
            return None
        return geometry

    def overlay_bounds(self):
        """Monitor-local intersection, in logical pixels (never physical pixels)."""
        left = max(0, self.x - self.monitor_x)
        top = max(0, self.y - self.monitor_y)
        right = min(self.monitor_width, self.x + self.width - self.monitor_x)
        bottom = min(self.monitor_height, self.y + self.height - self.monitor_y)
        return tuple(round(value) for value in (left, top, right - left, bottom - top))

    def normalize(self, x, y, clamp=False):
        left, top, width, height = self.overlay_bounds()
        if not clamp and not (0 <= x <= width and 0 <= y <= height):
            return None
        x, y = min(width, max(0, x)), min(height, max(0, y))
        nx = round((x + left + self.monitor_x - self.x) / self.width * 10000)
        ny = round((y + top + self.monitor_y - self.y) / self.height * 10000)
        return min(10000, max(0, nx)), min(10000, max(0, ny))

    def denormalize(self, x, y):
        left, top, _, _ = self.overlay_bounds()
        return (x / 10000 * self.width + self.x - self.monitor_x - left,
                y / 10000 * self.height + self.y - self.monitor_y - top)

    def frame_size(self):
        return round(self.width * self.scale), round(self.height * self.scale)


class RecordingClock:
    def __init__(self):
        self.started_at = time.monotonic()
        self.milliseconds = 0
        self.running = False

    def update(self, status, now=None):
        self.started_at = time.monotonic() if now is None else now
        self.milliseconds = max(0, int(status.get("elapsedMs", 0)))
        self.running = status.get("state") == "recording"

    def elapsed(self, now=None):
        now = time.monotonic() if now is None else now
        delta = max(0, now - self.started_at) * 1000 if self.running else 0
        return self.milliseconds + int(delta)


def new_recording(previous, status):
    return status.get("state") == "recording" and (
        previous.get("state") != "recording"
        or status.get("sessionId") != previous.get("sessionId")
        or status.get("elapsedMs", 0) + 250 < previous.get("elapsedMs", 0)
    )


def bundle_arguments(action, status):
    """Open or copy this session's saved capture, including custom output paths."""
    arguments = [action, status.get("bundle") or "latest"]
    if action == "send":
        arguments.append("--clipboard")
    return arguments


class DrawingFlush:
    """Finish live ink before acknowledging an external Stop or Draw-off."""
    def __init__(self):
        self.waiting = False
        self.session = None

    def handle(self, status, finish_stroke, send_command):
        session = status.get("sessionId")
        if session != self.session or not status.get("flushRequested", False):
            self.waiting = False
        self.session = session
        if not status.get("flushRequested", False) or self.waiting:
            return
        self.waiting = True
        # Both callbacks append to the same serialized command worker. A stroke
        # must reach Rust before the acknowledgement lets it stop the recorder.
        finish_stroke()
        send_command("drawing-flushed")


class StatusOrder:
    """Command acknowledgements supersede polls launched before they completed."""
    def __init__(self):
        self.issued = 0
        self.applied = 0

    def issue(self):
        self.issued += 1
        return self.issued

    def accept(self, sequence, command=False):
        if command:
            # Commands execute serially, but may have waited in the worker queue
            # while a newer poll read the daemon's older state.
            sequence = self.issue()
        if sequence <= self.applied:
            return False
        self.applied = sequence
        return True


def request(path, payload, timeout=3):
    """One newline-delimited JSON exchange on a local Unix socket."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(timeout)
        connection.connect(path)
        connection.sendall(json.dumps(payload, separators=(",", ":")).encode() + b"\n")
        with connection.makefile("rb") as stream:
            response = stream.readline(8 * 1024 * 1024 + 1)
        if len(response) > 8 * 1024 * 1024:
            raise ValueError("Scrawl status response is too large")
        if not response.endswith(b"\n"):
            raise ValueError("Scrawl closed the control connection without a complete response")
        value = json.loads(response)
        if not isinstance(value, dict):
            raise ValueError("Scrawl returned an invalid status response")
        return value


def load_gtk():
    # Layer Shell must interpose libwayland-client before GTK loads it.
    import ctypes
    ctypes.CDLL("libgtk4-layer-shell.so.0")
    import gi
    gi.require_version("Gtk", "4.0")
    gi.require_version("Gdk", "4.0")
    gi.require_version("Gtk4LayerShell", "1.0")
    gi.require_version("Gsk", "4.0")
    gi.require_foreign("cairo")
    from gi.repository import Gdk, Gio, GLib, Gsk, Gtk, Gtk4LayerShell
    import cairo
    return Gtk, Gdk, Gio, GLib, Gtk4LayerShell, cairo, Gsk


CSS = """
window.scrawl, window.scrawl-ink { background: transparent; }
.pill {
  color: #f5f5f7; background: #202125; border: 1px solid #45464e;
  border-radius: 23px; padding: 8px 10px; font-family: sans-serif;
  box-shadow: 0 4px 16px alpha(black, 0.35);
}
.brand { font-weight: 700; padding: 0 8px; }
.brand, .recording-dot { color: #ff5a63; }
.pill button {
  color: #f5f5f7; background: transparent; border: none; border-radius: 15px;
  box-shadow: none; padding: 7px 11px; min-height: 20px; min-width: 16px;
}
.pill button:hover { background: #3b3c43; }
.pill button:checked { background: #4a3339; color: #ffbdc1; }
.pill button:disabled { opacity: 0.38; }
.pill button.primary { background: #e84450; color: white; font-weight: 700; }
.pill button.primary:hover { background: #f65b66; }
.pill button.primary:disabled { background: #75434a; }
.pill .modes { background: #2d2e34; border-radius: 16px; }
.elapsed { font-family: monospace; font-size: 14px; padding: 0 10px 0 4px; }
.hint { color: #b8b9c1; font-size: 12px; padding: 5px 8px 2px; }
.hint.error { color: #ffb2b8; }
.status { padding-left: 8px; }
"""


def run_ui(args, modules):
    Gtk, Gdk, Gio, GLib, Layer, cairo, Gsk = modules

    class InkWindow:
        def __init__(self, owner, geometry, monitor):
            self.owner, self.geometry = owner, geometry
            self.current = None
            self.pending = []
            self.window = Gtk.Window(application=owner.app, title="Scrawl Draw")
            self.window.add_css_class("scrawl-ink")
            self.window.set_decorated(False)
            self.window.set_resizable(False)
            Layer.init_for_window(self.window)
            Layer.set_namespace(self.window, "scrawl-draw")
            Layer.set_layer(self.window, Layer.Layer.TOP)
            Layer.set_monitor(self.window, monitor)
            Layer.set_exclusive_zone(self.window, -1)
            Layer.set_keyboard_mode(self.window, Layer.KeyboardMode.EXCLUSIVE)
            Layer.set_anchor(self.window, Layer.Edge.TOP, True)
            Layer.set_anchor(self.window, Layer.Edge.LEFT, True)
            left, top, width, height = geometry.overlay_bounds()
            Layer.set_margin(self.window, Layer.Edge.LEFT, left)
            Layer.set_margin(self.window, Layer.Edge.TOP, top)
            self.window.set_default_size(width, height)
            self.canvas = Gtk.DrawingArea()
            self.canvas.set_draw_func(self.paint)
            self.canvas.set_focusable(True)
            self.canvas.set_cursor_from_name("crosshair")
            self.window.set_child(self.canvas)
            drag = Gtk.GestureDrag()
            drag.set_button(1)
            drag.connect("drag-begin", self.begin)
            drag.connect("drag-update", self.move)
            drag.connect("drag-end", self.end)
            self.canvas.add_controller(drag)
            keys = Gtk.EventControllerKey()
            keys.connect("key-pressed", self.key)
            self.window.add_controller(keys)
            self.window.connect("close-request", self.close_requested)
            self.window.present()
            self.canvas.grab_focus()

        def close_requested(self, _window):
            self.owner.command("draw", enabled=False)
            return True

        def key(self, _controller, keyval, _keycode, modifiers):
            if keyval == Gdk.KEY_Escape:
                self.finish_stroke()
                self.owner.command("draw", enabled=False)
                return True
            if modifiers & Gdk.ModifierType.CONTROL_MASK and keyval in (Gdk.KEY_z, Gdk.KEY_Z):
                self.current = None
                self.owner.command("undo")
                self.canvas.queue_draw()
                return True
            return False

        def begin(self, _gesture, x, y):
            if self.owner.flush.waiting:
                return
            point = self.geometry.normalize(x, y)
            if point is None:
                return
            self.origin = (x, y)
            fw, fh = self.geometry.frame_size()
            now = self.owner.clock.elapsed()
            self.current = {"t0": now, "t1": now, "fw": fw, "fh": fh,
                            "pts": [*point, 0], "laser": False}
            self.canvas.queue_draw()

        def move(self, _gesture, dx, dy):
            if self.current is None:
                return
            point = self.geometry.normalize(self.origin[0] + dx, self.origin[1] + dy, clamp=True)
            if point == tuple(self.current["pts"][-3:-1]):
                return
            now = max(self.current["t1"], self.owner.clock.elapsed())
            self.current["t1"] = now
            self.current["pts"].extend((*point, now - self.current["t0"]))
            self.canvas.queue_draw()

        def end(self, gesture, dx, dy):
            self.move(gesture, dx, dy)
            self.finish_stroke()

        def finish_stroke(self):
            if self.current:
                self.current["t1"] = max(self.current["t1"], self.owner.clock.elapsed())
                stroke, self.current = self.current, None
                self.pending.append(stroke)
                self.owner.command("stroke", stroke=stroke,
                                   on_done=lambda _response: self.acknowledge(stroke))

        def acknowledge(self, stroke):
            if stroke in self.pending:
                self.pending.remove(stroke)
            self.canvas.queue_draw()

        def paint(self, _area, context, _width, _height):
            context.set_operator(cairo.OPERATOR_SOURCE)
            context.set_source_rgba(0, 0, 0, 0)
            context.paint()
            context.set_operator(cairo.OPERATOR_OVER)
            context.set_source_rgba(1, 0.20, 0.26, 0.96)
            context.set_line_width(4)
            context.set_line_cap(cairo.LINE_CAP_ROUND)
            context.set_line_join(cairo.LINE_JOIN_ROUND)
            strokes = list(self.owner.status.get("strokes") or [])
            strokes.extend(stroke for stroke in self.pending if stroke not in strokes)
            if self.current:
                strokes.append(self.current)
            for stroke in strokes:
                # Ending Draw clears visible ink; old marks remain in bundle metadata.
                if stroke.get("t0", 0) < self.owner.ink_since:
                    continue
                points = stroke.get("pts", [])
                if len(points) < 3:
                    continue
                x, y = self.geometry.denormalize(points[0], points[1])
                if len(points) == 3:
                    context.arc(x, y, 2, 0, math.tau)
                    context.fill()
                    continue
                context.move_to(x, y)
                for index in range(3, len(points) - 2, 3):
                    context.line_to(*self.geometry.denormalize(points[index], points[index + 1]))
                context.stroke()

        def destroy(self):
            self.current = None
            self.window.destroy()

    class Controls:
        def __init__(self, app):
            self.app = app
            self.clock = RecordingClock()
            self.status = {"state": "idle", "bundle": None, "drawing": False}
            self.mode = "screen"
            self.ink = None
            self.ink_since = 0
            self.polling = False
            self.pending = 0
            self.order = StatusOrder()
            self.flush = DrawingFlush()
            self.closed = False
            self.notice = None
            self.notice_until = 0
            self.connected = False
            self.was_connected = False
            self.disconnects = 0
            self.commands = ThreadPoolExecutor(max_workers=1, thread_name_prefix="scrawl-command")
            self.polls = ThreadPoolExecutor(max_workers=1, thread_name_prefix="scrawl-status")
            self.utilities = ThreadPoolExecutor(max_workers=1, thread_name_prefix="scrawl-preview")
            self.window = Gtk.ApplicationWindow(application=app, title="Scrawl")
            self.window.add_css_class("scrawl")
            self.window.set_decorated(False)
            self.window.set_resizable(False)
            if not Layer.is_supported():
                raise RuntimeError("Scrawl needs a Wayland compositor with layer-shell support (Hyprland)")
            Layer.init_for_window(self.window)
            Layer.set_namespace(self.window, "scrawl-controls")
            Layer.set_layer(self.window, Layer.Layer.OVERLAY)
            Layer.set_anchor(self.window, Layer.Edge.TOP, True)
            Layer.set_margin(self.window, Layer.Edge.TOP, 40)
            Layer.set_exclusive_zone(self.window, -1)
            Layer.set_keyboard_mode(self.window, Layer.KeyboardMode.ON_DEMAND)
            provider = Gtk.CssProvider()
            provider.load_from_string(CSS)
            Gtk.StyleContext.add_provider_for_display(
                Gdk.Display.get_default(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
            self.shell = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
            self.shell.add_css_class("pill")
            row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=5)
            self.shell.append(row)
            brand = Gtk.Label(label="● Scrawl")
            brand.add_css_class("brand")
            row.append(brand)
            self.modes = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
            self.modes.add_css_class("modes")
            group = None
            for mode in ("screen", "window", "area"):
                button = Gtk.ToggleButton(label=mode.title())
                if group:
                    button.set_group(group)
                else:
                    group = button
                button.set_active(mode == self.mode)
                button.connect("toggled", self.select_mode, mode)
                self.modes.append(button)
            row.append(self.modes)
            self.microphone = Gtk.ToggleButton(label="Mic on", active=True)
            self.microphone.set_tooltip_text("Include the default microphone in this recording")
            self.microphone.connect("toggled", lambda button: button.set_label(
                "Mic on" if button.get_active() else "Mic off"))
            row.append(self.microphone)
            self.state_label = Gtk.Label(label="Ready")
            self.state_label.add_css_class("status")
            row.append(self.state_label)
            self.timer = Gtk.Label(label="0:00")
            self.timer.add_css_class("elapsed")
            row.append(self.timer)
            self.primary = self.button(row, "Record", self.record, "Start recording")
            self.primary.add_css_class("primary")
            self.draw_button = Gtk.ToggleButton(label="Draw")
            self.draw_button.set_tooltip_text("Draw red ink over the captured area; Escape exits Draw")
            self.draw_button.connect("clicked", self.toggle_draw)
            row.append(self.draw_button)
            self.undo = self.button(row, "Undo", lambda _button: self.command("undo"), "Undo the last stroke (Ctrl+Z in Draw)")
            self.cancel = self.button(row, "Cancel", lambda _button: self.command("cancel"), "Discard this recording")
            self.preview = self.button(row, "Preview", lambda _button: self.utility("preview"), "Open the saved capture's contact sheet")
            self.copy = self.button(row, "Copy", lambda _button: self.utility("send"), "Copy the saved capture's plain-text summary and file paths")
            self.close = self.button(row, "×", lambda _button: self.window.close(), "Close Scrawl controls")
            self.hint = Gtk.Label(wrap=True, max_width_chars=82, xalign=0)
            self.hint.add_css_class("hint")
            self.shell.append(self.hint)
            self.window.set_child(self.shell)
            self.window.connect("close-request", self.close_requested)
            self.refresh()
            self.window.present()
            self.poll()
            GLib.timeout_add(250, self.poll)
            GLib.timeout_add(100, self.tick)

        def button(self, row, label, callback, tooltip):
            button = Gtk.Button(label=label)
            button.set_tooltip_text(tooltip)
            button.connect("clicked", callback)
            row.append(button)
            return button

        def select_mode(self, button, mode):
            if button.get_active():
                self.mode = mode

        def launch(self, executor, payload, callback, timeout):
            sequence = self.order.issue()
            future = executor.submit(request, args.socket, payload, timeout)

            def completed(result):
                try:
                    value = result.result()
                except (FileNotFoundError, ConnectionRefusedError):
                    value = {"ok": False, "error": "Recorder disconnected. Run scrawl app to reconnect."}
                except Exception as error:
                    value = {"ok": False, "error": str(error)}
                GLib.idle_add(callback, sequence, value)

            future.add_done_callback(completed)

        def poll(self):
            if self.closed:
                return False
            if not self.polling:
                self.polling = True
                self.launch(self.polls, {"command": "status", "client": "ui"}, self.polled, 2)
            return True

        def polled(self, sequence, response):
            self.polling = False
            self.connected = "state" in response
            if self.connected:
                self.was_connected = True
                self.disconnects = 0
            else:
                self.disconnects += 1
            if self.was_connected and self.disconnects >= 3:
                self.window.close()
                return False
            self.apply(sequence, response)
            return False

        def command(self, name, on_done=None, **fields):
            if self.closed:
                return
            self.pending += 1
            self.notice = None
            if name in ("stop", "cancel", "draw") and self.ink:
                self.ink.finish_stroke()
            if name == "stop" or (name == "draw" and fields.get("enabled") is False):
                fields["flushed"] = True
            self.refresh()

            def completed(sequence, response):
                self.pending -= 1
                self.apply(sequence, response, command=True)
                if on_done:
                    on_done(response)
                self.refresh()
                return False

            self.launch(self.commands, {"command": name, **fields}, completed, 120)

        def apply(self, sequence, response, command=False):
            if self.closed:
                return
            if not response.get("ok", False):
                self.notice = response.get("error", "Scrawl could not complete this action")
                self.notice_until = time.monotonic() + 6
            if "state" in response and self.order.accept(sequence, command=command):
                was_drawing = self.status.get("drawing", False)
                is_new_recording = new_recording(self.status, response)
                if is_new_recording and self.ink:
                    self.ink.destroy()
                    self.ink = None
                self.status = response
                self.clock.update(response)
                if is_new_recording:
                    self.ink_since = 0
                elif was_drawing and not response.get("drawing", False):
                    self.ink_since = self.clock.elapsed()
                self.flush.handle(response, self.finish_stroke, self.command)
            self.refresh()

        def finish_stroke(self):
            if self.ink:
                self.ink.finish_stroke()

        def record(self, _button):
            if self.status.get("state") == "recording":
                self.command("stop")
            else:
                self.command("start", mode=self.mode, noAudio=not self.microphone.get_active())

        def toggle_draw(self, _button):
            enabled = not self.status.get("drawing", False)
            self.command("draw", enabled=enabled)

        def monitor(self):
            name = (self.status.get("monitor") or {}).get("name")
            monitors = Gdk.Display.get_default().get_monitors()
            for index in range(monitors.get_n_items()):
                monitor = monitors.get_item(index)
                if monitor.get_connector() == name:
                    return monitor
            return None

        def refresh(self):
            if self.closed:
                return
            state = self.status.get("state", "idle")
            recording, processing = state == "recording", state in ("processing", "starting")
            drawing = recording and self.connected and self.status.get("drawing", False)
            busy = bool(self.pending)
            self.modes.set_visible(not recording and not processing)
            self.modes.set_sensitive(not busy)
            self.microphone.set_visible(not recording and not processing)
            self.microphone.set_sensitive(not busy)
            self.primary.set_label("Stop" if recording else "Starting…" if state == "starting" else "Processing…" if processing else "Record")
            self.primary.set_sensitive(self.connected and not busy and not processing)
            self.primary.set_tooltip_text("Finish and save the recording" if recording else "Start recording")
            self.state_label.set_visible(recording or processing)
            self.state_label.set_label("● Recording" if recording else "Preparing…" if state == "starting" else "Building capture…")
            self.timer.set_visible(recording or processing)
            self.draw_button.set_visible(recording)
            self.draw_button.set_sensitive(self.connected and not busy)
            self.draw_button.set_active(drawing)
            self.undo.set_visible(drawing)
            self.undo.set_sensitive(not busy and bool(self.status.get("strokes")))
            self.cancel.set_visible(recording)
            self.cancel.set_sensitive(self.connected and not busy)
            self.preview.set_visible(not recording and not processing)
            self.copy.set_visible(not recording and not processing)
            self.close.set_visible(not recording and not processing)
            monitor = self.monitor()
            if monitor:
                Layer.set_monitor(self.window, monitor)
            geometry = CaptureGeometry.from_status(self.status)
            if self.ink and (not drawing or self.ink.geometry != geometry):
                self.ink.destroy()
                self.ink = None
            if drawing and self.ink is None and geometry and monitor:
                self.ink = InkWindow(self, geometry, monitor)
            if self.ink:
                self.ink.canvas.queue_draw()
            error = self.status.get("error")
            if self.notice and time.monotonic() < self.notice_until:
                error = self.notice
            if error:
                hint = error
            elif drawing:
                hint = "Draw on the capture · Ctrl+Z to undo · Esc to return to your apps"
                if not geometry or not monitor:
                    hint = "The captured monitor is unavailable. Turn Draw off to continue."
            elif state == "starting":
                hint = "Choose the capture area…" if self.mode == "area" else "Starting recording…"
            elif processing:
                hint = "Saving your video, frames and summary…"
            elif busy and not recording:
                hint = "Choose an area on your screen…" if self.mode == "area" else "Starting recording…"
            elif self.status.get("bundle") and not recording:
                hint = "Capture saved · Preview the frames or copy the summary"
            else:
                hint = ""
            self.hint.set_label(hint)
            self.hint.set_visible(bool(hint))
            if error:
                self.hint.add_css_class("error")
            else:
                self.hint.remove_css_class("error")

        def tick(self):
            if self.closed:
                return False
            seconds = self.clock.elapsed() // 1000
            self.timer.set_label(f"{seconds // 60}:{seconds % 60:02d}")
            if self.notice and time.monotonic() >= self.notice_until:
                self.notice = None
                self.refresh()
            return True

        def utility(self, action):
            arguments = bundle_arguments(action, self.status)
            self.preview.set_sensitive(False)
            self.copy.set_sensitive(False)
            future = self.utilities.submit(subprocess.run, [args.binary, *arguments],
                                           capture_output=True, text=True, timeout=20)

            def finished(result):
                try:
                    process = result.result()
                    message = None if process.returncode == 0 else process.stderr.strip() or "Scrawl could not open this capture"
                except Exception as error:
                    message = str(error)
                GLib.idle_add(self.utility_done, message, arguments[0] == "send")

            future.add_done_callback(finished)

        def utility_done(self, error, copied):
            if self.closed:
                return False
            self.preview.set_sensitive(True)
            self.copy.set_sensitive(True)
            if error or copied:
                self.notice = error or "Summary copied to the clipboard"
                self.notice_until = time.monotonic() + 4
                self.refresh()
                if not error:
                    self.hint.remove_css_class("error")
            return False

        def close_requested(self, _window):
            # The daemon can keep recording after the controls are closed, but
            # it must never believe an absent drawing surface still owns input.
            if self.connected and self.status.get("drawing"):
                if self.ink:
                    self.ink.finish_stroke()
                self.commands.submit(request, args.socket, {"command": "draw", "enabled": False, "flushed": True})
            self.closed = True
            if self.ink:
                self.ink.destroy()
            for executor in (self.commands, self.polls, self.utilities):
                executor.shutdown(wait=False, cancel_futures=False)
            self.app.quit()
            return False

        def render(self):
            """Save the native controls for a visual bug report, even under a lock screen."""
            try:
                paintable = Gtk.WidgetPaintable.new(self.shell)
                snapshot = Gtk.Snapshot()
                paintable.snapshot(snapshot, self.shell.get_width(), self.shell.get_height())
                node = snapshot.to_node()
                if node is None:
                    raise RuntimeError("The controls have not been drawn")
                renderer = Gsk.Renderer.new_for_surface(self.window.get_surface())
                try:
                    texture = renderer.render_texture(node, None)
                    if not texture.save_to_png(args.render):
                        raise RuntimeError(f"Could not save {args.render}")
                finally:
                    renderer.unrealize()
            except Exception as error:
                errors.append(str(error))
                print(f"Scrawl controls: {error}", file=sys.stderr)
            self.window.close()
            return False

    # Repeated CLI toggles should raise one pill, never stack input surfaces.
    identity = hashlib.sha256(args.socket.encode()).hexdigest()[:16]
    flags = Gio.ApplicationFlags.NON_UNIQUE if args.smoke or args.render else Gio.ApplicationFlags.DEFAULT_FLAGS
    app = Gtk.Application(application_id=f"dev.kitlangton.scrawl.linux.s{identity}", flags=flags)
    controls = []
    errors = []

    def activate(application):
        try:
            if controls:
                controls[0].window.present()
                return
            controls.append(Controls(application))
            if args.render:
                GLib.timeout_add(700, controls[0].render)
            elif args.smoke:
                GLib.timeout_add(1200, lambda: controls[0].window.close() or False)
        except Exception as error:
            errors.append(str(error))
            print(f"Scrawl controls: {error}", file=sys.stderr)
            application.quit()

    app.connect("activate", activate)
    code = app.run([])
    return 1 if errors else code


def main(argv=None):
    parser = argparse.ArgumentParser(description="Scrawl native Linux controls")
    parser.add_argument("--binary", help="Absolute path to the scrawl executable")
    parser.add_argument("--socket", help="Absolute path to the daemon control socket")
    parser.add_argument("--check", action="store_true", help="Check UI dependencies without opening a window")
    parser.add_argument("--smoke", action="store_true", help="Open controls briefly, then exit")
    parser.add_argument("--render", metavar="PNG", help="Save a PNG of the native controls, then exit")
    args = parser.parse_args(argv)
    try:
        modules = load_gtk()
    except (ImportError, ValueError, OSError) as error:
        print(f"Scrawl controls need Python PyGObject, Pycairo, GTK4 and gtk4-layer-shell.\n"
              f"On Arch: sudo pacman -S python-gobject python-cairo gtk4 gtk4-layer-shell\n{error}", file=sys.stderr)
        return 1
    if args.check:
        print("GTK4, PyGObject, Pycairo and gtk4-layer-shell are available")
        return 0
    if not args.binary or not args.socket:
        parser.error("--binary and --socket are required unless --check is used")
    return run_ui(args, modules)


if __name__ == "__main__":
    raise SystemExit(main())
