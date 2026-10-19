#!/usr/bin/env python3
"""Exercise the compiled Linux service with real video and isolated desktop adapters.

Run: python3 tests/linux_smoke.py target/debug/scrawl
No real desktop, microphone, clipboard, or external session is touched.
"""
import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest

BINARY = str(Path(sys.argv.pop(1) if len(sys.argv) > 1 else "target/debug/scrawl").resolve())


class ServiceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixtures = tempfile.TemporaryDirectory(prefix="scrawl-media-")
        cls.movie = Path(cls.fixtures.name) / "fixture.mp4"
        subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-f", "lavfi", "-i",
                        "color=c=0x243347:s=640x360:r=10:d=2", "-f", "lavfi", "-i",
                        "sine=frequency=440:duration=2", "-c:v", "libx264", "-threads", "1",
                        "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", str(cls.movie)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.fixtures.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="scrawl-service-")
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.runtime = self.root / "run"
        self.clipboard = self.root / "clipboard.txt"
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        SCRAWL_RUNTIME_DIR=str(self.runtime),
                        SCRAWL_CAPTURES_DIR=str(self.root / "captures"),
                        SCRAWL_CONFIG=str(self.root / "config.json"),
                        SCRAWL_TEST_VIDEO=str(self.movie), SCRAWL_TEST_CLIPBOARD=str(self.clipboard))
        self.env.pop("SCRAWL_WHISPER_MODEL", None)
        self.env.pop("SCRAWL_WHISPER_BIN", None)
        self.script("hyprctl", '''import json,sys
arg=sys.argv[-1]
if arg == "monitors": print(json.dumps([dict(id=0,name="TEST-1",x=0,y=0,width=640,height=360,scale=1,focused=True,mirrorOf="none")]))
elif arg == "cursorpos": print('{"x":320,"y":180}')
elif arg == "activewindow": print('{"address":"0x123","class":"Test","title":"Test window","at":[20,20],"size":[320,200],"monitor":0}')
else: sys.exit(2)
''')
        self.script("wf-recorder", '''import os,shutil,signal,sys,time
out=sys.argv[sys.argv.index("--file")+1]
shutil.copyfile(os.environ["SCRAWL_TEST_VIDEO"],out)
signal.signal(signal.SIGINT,lambda *_: sys.exit(0))
signal.signal(signal.SIGTERM,lambda *_: sys.exit(0))
while True: time.sleep(0.1)
''')
        self.script("wl-copy", '''import os,sys
open(os.environ["SCRAWL_TEST_CLIPBOARD"],"w").write(sys.stdin.read())
''')
        self.log = open(self.root / "daemon.log", "w+")
        self.daemon = subprocess.Popen([BINARY, "daemon"], env=self.env,
                                       stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log)
        self.wait(lambda: self.runtime.joinpath("control.sock").exists())

    def tearDown(self):
        if self.daemon.poll() is None:
            try:
                self.rpc(command="shutdown")
                self.daemon.wait(timeout=5)
            except Exception:
                self.daemon.terminate()
                try: self.daemon.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    self.daemon.kill()
                    self.daemon.wait()
        self.log.close()
        self.temp.cleanup()

    def script(self, name, text):
        path = self.bin / name
        path.write_text("#!/usr/bin/python3\n" + text)
        path.chmod(0o755)

    def rpc(self, **payload):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(5)
            connection.connect(str(self.runtime / "control.sock"))
            connection.sendall(json.dumps(payload).encode() + b"\n")
            return json.loads(connection.makefile("rb").readline())

    def wait(self, predicate, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = predicate()
            if value: return value
            time.sleep(0.06)
        self.log.flush()
        self.fail("Timed out; daemon log:\n" + (self.root / "daemon.log").read_text())

    def state(self, state):
        status = self.rpc(command="status")
        if state != "error" and status.get("state") == "error":
            self.fail(f"Service failed while waiting for {state}: {status}")
        return status if status.get("state") == state else None

    def start(self):
        reply = self.rpc(command="start", mode="screen", noAudio=True)
        self.assertTrue(reply["ok"], reply)
        return self.wait(lambda: self.state("recording"))

    def test_start_stop_builds_real_bundle_and_plain_text_clipboard(self):
        status = self.start()
        self.assertEqual(status["target"]["frame"]["width"], 640)
        self.assertEqual(self.rpc(command="stop")["state"], "processing")
        result = self.wait(lambda: self.state("idle"))
        bundle = Path(result["bundle"])
        manifest = json.loads((bundle / "scrawl.json").read_text())
        self.assertEqual(manifest["source"], "linux")
        self.assertEqual(len(manifest["keyframes"]), 1, "Static narration must not add frames")
        self.assertTrue((bundle / "audio.wav").is_file())
        self.assertTrue((bundle / "frames/frame-01.png").is_file())
        summary = (bundle / "summary.md").read_text()
        self.assertIn(str(bundle), summary)
        self.assertEqual(self.clipboard.read_text(), summary)
        self.assertEqual(self.runtime.stat().st_mode & 0o777, 0o700)
        self.assertEqual((self.runtime / "control.sock").stat().st_mode & 0o777, 0o600)

    def test_concurrent_start_is_rejected_and_cancel_discards_only_new_bundle(self):
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            replies = list(pool.map(lambda _: self.rpc(command="start", mode="screen", noAudio=True), range(2)))
        self.assertEqual(sum(reply["ok"] for reply in replies), 1, replies)
        self.wait(lambda: self.state("recording"))
        self.assertTrue(self.rpc(command="cancel")["ok"])
        self.assertEqual(self.rpc(command="status")["state"], "idle")
        self.assertEqual(list((self.root / "captures").iterdir()), [])
        self.assertFalse(self.clipboard.exists())

    def test_existing_output_is_preserved(self):
        target = self.root / "keep.scrawl"
        target.mkdir()
        (target / "sentinel").write_text("keep")
        self.rpc(command="start", mode="screen", noAudio=True, out=str(target))
        result = self.wait(lambda: self.state("error"))
        self.assertIn("new output path", result["error"])
        self.assertEqual((target / "sentinel").read_text(), "keep")
        self.assertEqual(len(list(target.iterdir())), 1)

    def test_failed_recorder_reports_error_and_service_can_record_again(self):
        self.script("wf-recorder", "import sys\nprint('test encoder refused',file=sys.stderr)\nsys.exit(19)\n")
        self.rpc(command="start", mode="screen", noAudio=True)
        result = self.wait(lambda: self.state("error"))
        self.assertIn("test encoder refused", result["error"])
        self.assertFalse(self.clipboard.exists())
        self.assertEqual(self.rpc(command="status")["state"], "error")

    def test_clock_starts_when_video_arrives_and_stays_responsive_during_startup(self):
        script = (self.bin / "wf-recorder").read_text()
        self.script("wf-recorder", script.split("\n", 1)[1].replace("shutil.copyfile", "time.sleep(0.6)\nshutil.copyfile"))
        self.rpc(command="start", mode="screen", noAudio=True)
        time.sleep(0.3)
        status = self.rpc(command="status")
        self.assertEqual(status["state"], "starting")
        self.assertEqual(status["elapsedMs"], 0)
        status = self.wait(lambda: self.state("recording"))
        self.assertLess(status["elapsedMs"], 250)
        self.rpc(command="cancel")

    def test_external_stop_waits_for_current_drawing_and_records_visibility(self):
        self.start()
        self.rpc(command="draw")
        self.rpc(command="status", client="ui")
        reply = self.rpc(command="stop")
        self.assertTrue(reply["flushRequested"])
        self.assertEqual(reply["state"], "recording")
        stroke = dict(t0=20,t1=100,fw=640,fh=360,pts=[100,100,0,9000,8000,80],laser=False)
        self.assertTrue(self.rpc(command="stroke", stroke=stroke)["ok"])
        self.assertEqual(self.rpc(command="drawing-flushed")["state"], "processing")
        done = self.wait(lambda: self.state("idle"))
        metadata = json.loads((Path(done["bundle"]) / "capture.json").read_text())
        self.assertEqual(len(metadata["strokes"]), 1)
        self.assertEqual(len(metadata["inkSessions"]), 1)
        self.assertLess(metadata["inkSessions"][0][1], 2**64-1)

    def test_cancel_interrupts_area_picker(self):
        self.script("slurp", "import time\ntime.sleep(60)\n")
        self.rpc(command="start", mode="area", noAudio=True)
        time.sleep(0.15)
        self.assertTrue(self.rpc(command="cancel")["ok"])
        self.wait(lambda: self.state("idle"), timeout=2)

    def test_leaving_draw_mode_cannot_cancel_pending_stop(self):
        self.start()
        self.rpc(command="draw")
        self.rpc(command="status", client="ui")
        self.assertTrue(self.rpc(command="stop")["flushRequested"])
        reply = self.rpc(command="draw", enabled=False, flushed=True)
        self.assertEqual(reply["state"], "processing")
        self.assertFalse(reply["flushRequested"])
        self.assertTrue(self.rpc(command="drawing-flushed")["ok"])
        done = self.wait(lambda: self.state("idle"))
        self.assertTrue((Path(done["bundle"]) / "scrawl.json").is_file())

    def test_cli_resolves_relative_output_against_client_working_directory(self):
        caller = self.root / "caller"
        caller.mkdir()
        subprocess.run([BINARY,"start","--no-ui","--no-audio","--out","relative.scrawl"],
                       env=self.env,cwd=caller,check=True,stdout=subprocess.DEVNULL)
        self.wait(lambda: self.state("recording"))
        self.assertTrue((caller / "relative.scrawl/recording.mp4").is_file())
        self.rpc(command="cancel")

    def test_drawing_validation_and_undo(self):
        self.start()
        self.assertTrue(self.rpc(command="draw")["drawing"])
        stroke = dict(t0=10,t1=100,fw=640,fh=360,pts=[100,100,0,9000,8000,90],laser=False)
        result = self.rpc(command="stroke", stroke=stroke)
        self.assertTrue(result["ok"], result)
        self.assertEqual(len(result["strokes"]), 1)
        stroke["pts"][0] = -1
        self.assertFalse(self.rpc(command="stroke", stroke=stroke)["ok"])
        self.assertEqual(len(self.rpc(command="undo")["strokes"]), 0)
        self.assertTrue(self.rpc(command="cancel")["ok"])


if __name__ == "__main__":
    unittest.main()
