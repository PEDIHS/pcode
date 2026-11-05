"""MCP stdio proxy that adds the Jcode agent overlay to Windows-MCP.

Usage (as an MCP server command):

    python proxy.py -- <real server command ...>

Every JSON-RPC message is passed through unchanged, except:

* ``tools/call`` for Click / Move / Type / Scroll / MultiSelect / MultiEdit:
  the purple agent cursor first glides to the target (``loc``), then the call
  is forwarded; clicks get a ripple.  Calls that use ``label`` instead of
  ``loc`` are resolved from the last Snapshot output when possible, otherwise
  the ripple is drawn where the real mouse ended up.
* ``Screenshot`` / ``Snapshot``: the overlay is hidden for the capture so the
  model sees the real screen (set JCODE_OVERLAY_IN_CAPTURE=1 to keep it).
* Stop (real Esc key or a real click on the stop pill): every in-flight call
  is answered with an error and cancelled in the child, and new desktop
  actions are refused for JCODE_STOP_SECONDS (default 60) so the agent has to
  stop and ask the user.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

LOG_PATH = os.environ.get("JCODE_OVERLAY_LOG")
VISUAL_TOOLS = {"Click", "Move", "Type", "Scroll", "MultiSelect", "MultiEdit"}
CAPTURE_TOOLS = {"Screenshot", "Snapshot"}
ACTION_TOOLS = VISUAL_TOOLS | {"Shortcut", "App", "Clipboard"}
STOP_SECONDS = float(os.environ.get("JCODE_STOP_SECONDS", "60"))
KEEP_IN_CAPTURE = os.environ.get("JCODE_OVERLAY_IN_CAPTURE", "") in {"1", "true", "yes"}


def log(text: str) -> None:
    if LOG_PATH:
        try:
            with open(LOG_PATH, "a", encoding="utf-8") as fh:
                fh.write(time.strftime("%H:%M:%S ") + text + "\n")
        except OSError:
            pass


class Proxy:
    def __init__(self, child_cmd: list[str]):
        self.out_lock = threading.Lock()
        self.overlay = None
        self.overlay_ready = threading.Event()
        # Start the overlay first: the child (uvx + windows-mcp) is heavy and would
        # starve it on a low-memory machine.  Wait at most a few seconds.
        threading.Thread(target=self._start_overlay, daemon=True).start()
        self.overlay_ready.wait(float(os.environ.get("JCODE_OVERLAY_START_WAIT", "8")))
        self.child = subprocess.Popen(child_cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None,
                                      bufsize=0, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        self.child_lock = threading.Lock()
        self.pending: dict = {}           # request id -> info dict for tracked tools/call
        self.pending_lock = threading.Lock()
        self.stopped_until = 0.0
        self.cancelled: set = set()
        self.labels: dict[int, tuple[int, int]] = {}

    # ------------------------------------------------------------ overlay
    def _start_overlay(self) -> None:
        try:
            from overlay import Overlay
            self.overlay = Overlay(on_stop=self.on_stop)
            log("overlay ready")
        except Exception as e:  # the proxy must keep working without the overlay
            log(f"overlay failed: {e!r}")
        finally:
            self.overlay_ready.set()

    def on_stop(self) -> None:
        self.stopped_until = time.monotonic() + STOP_SECONDS
        with self.pending_lock:
            items = list(self.pending.items())
            self.pending.clear()
        log(f"STOP pressed; cancelling {len(items)} call(s)")
        for rid, info in items:
            self.cancelled.add(rid)
            self.send_child({"jsonrpc": "2.0", "method": "notifications/cancelled",
                             "params": {"requestId": rid, "reason": "Stopped by the user"}})
            self.reply_error(rid, "The user pressed STOP (Esc / stop button) on the Jcode overlay. "
                                  "This action was cancelled. Stop now and ask the user how to continue.")
            if info.get("visual") and self.overlay:
                self.overlay.busy(-1)

    # ------------------------------------------------------------ io
    def send_client(self, msg: dict) -> None:
        data = (json.dumps(msg, ensure_ascii=False) + "\n").encode("utf-8")
        with self.out_lock:
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()

    def send_child(self, msg) -> None:
        data = msg if isinstance(msg, bytes) else (json.dumps(msg, ensure_ascii=False) + "\n").encode("utf-8")
        with self.child_lock:
            try:
                self.child.stdin.write(data)
                self.child.stdin.flush()
            except OSError as e:
                log(f"child write failed: {e!r}")

    def reply_error(self, rid, text: str) -> None:
        self.send_client({"jsonrpc": "2.0", "id": rid,
                          "result": {"content": [{"type": "text", "text": text}], "isError": True}})

    # ------------------------------------------------------------ client -> child
    def client_loop(self) -> None:
        for raw in sys.stdin.buffer:
            line = raw.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except ValueError:
                self.send_child(raw if raw.endswith(b"\n") else raw + b"\n")
                continue
            if isinstance(msg, dict) and msg.get("method") == "tools/call" and "id" in msg:
                threading.Thread(target=self.handle_call, args=(msg,), daemon=True).start()
            else:
                self.send_child(msg)
        try:
            self.child.stdin.close()
        except OSError:
            pass

    def _point(self, args: dict, key: str = "loc"):
        loc = args.get(key)
        if isinstance(loc, str):
            try:
                loc = json.loads(loc)
            except ValueError:
                nums = re.findall(r"-?\d+", loc)
                loc = [int(n) for n in nums[:2]] if len(nums) >= 2 else None
        if isinstance(loc, (list, tuple)) and len(loc) >= 2:
            try:
                return int(loc[0]), int(loc[1])
            except (TypeError, ValueError):
                return None
        label = args.get("label")
        if label is not None:
            try:
                return self.labels.get(int(label))
            except (TypeError, ValueError):
                return None
        return None

    def handle_call(self, msg: dict) -> None:
        rid = msg["id"]
        params = msg.get("params") or {}
        name = params.get("name", "")
        args = params.get("arguments") or {}
        if name in VISUAL_TOOLS and not self.overlay_ready.is_set():
            self.overlay_ready.wait(5)
        ov = self.overlay
        if name in ACTION_TOOLS and time.monotonic() < self.stopped_until:
            self.reply_error(rid, "The user stopped the agent from the Jcode overlay a moment ago. "
                                  "Do not continue desktop actions; ask the user first.")
            return
        info = {"name": name, "visual": False, "args": args}
        log(f"call {name} overlay={'yes' if ov else 'no'}")
        if ov and name in VISUAL_TOOLS:
            info["visual"] = True
            ov.busy(+1)
            pts = []
            if name in ("MultiSelect", "MultiEdit"):
                for item in (args.get("locs") or []):
                    if isinstance(item, (list, tuple)) and len(item) >= 2:
                        pts.append((int(item[0]), int(item[1])))
            else:
                if name == "Move" and args.get("from_loc") is not None:
                    p0 = self._point(args, "from_loc")
                    if p0:
                        ov.move_to(*p0).wait(1.0)
                p = self._point(args)
                if p:
                    pts.append(p)
            if pts:
                ov.move_to(*pts[0]).wait(1.0)
                info["point"] = pts[-1]
            if name == "Type":
                if pts:
                    ov.click(*pts[0])
                ov.typing(True)
        elif ov and name in CAPTURE_TOOLS and not KEEP_IN_CAPTURE:
            info["capture"] = True
            ov.capture_hide(True).wait(0.5)
            time.sleep(0.06)  # let DWM drop the layered windows before the grab
        elif ov:
            ov.activity()
        with self.pending_lock:
            self.pending[rid] = info
        self.send_child(msg)

    # ------------------------------------------------------------ child -> client
    def child_loop(self) -> None:
        for raw in self.child.stdout:
            line = raw.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except ValueError:
                with self.out_lock:
                    sys.stdout.buffer.write(raw if raw.endswith(b"\n") else raw + b"\n")
                    sys.stdout.buffer.flush()
                continue
            rid = msg.get("id") if isinstance(msg, dict) else None
            info = None
            if rid is not None and ("result" in msg or "error" in msg):
                with self.pending_lock:
                    info = self.pending.pop(rid, None)
                if info is None and self._was_cancelled(rid):
                    continue
            if info:
                self.after_call(info, msg)
            self.send_client(msg)
        log("child exited")
        os._exit(0)

    def _was_cancelled(self, rid) -> bool:
        # responses for calls we already answered after STOP are dropped
        if rid in self.cancelled:
            self.cancelled.discard(rid)
            return True
        return False

    def after_call(self, info: dict, msg: dict) -> None:
        ov = self.overlay
        name = info["name"]
        if name == "Snapshot":
            self._learn_labels(msg)
        if not ov:
            return
        if info.get("capture"):
            ov.capture_hide(False)
            ov.activity()
            return
        if not info.get("visual"):
            ov.activity()
            return
        if name == "Type":
            ov.typing(False)
        elif name == "Click":
            p = info.get("point")
            if p is None:
                from overlay import cursor_pos
                p = cursor_pos()
            ov.click(*p)
        elif name in ("Move", "Scroll") and info.get("point") is None:
            from overlay import cursor_pos
            ov.move_to(*cursor_pos())
        ov.busy(-1)

    def _learn_labels(self, msg: dict) -> None:
        """Best effort: remember label -> centre point from a Snapshot result."""
        try:
            texts = [c.get("text", "") for c in msg.get("result", {}).get("content", []) if c.get("type") == "text"]
        except AttributeError:
            return
        found: dict[int, tuple[int, int]] = {}
        for text in texts:
            for line in text.splitlines():
                m = re.match(r"\s*\|?\s*(\d+)\s*[|,]", line)
                if not m:
                    continue
                coords = re.findall(r"\(\s*(-?\d+)\s*,\s*(-?\d+)\s*\)", line)
                if coords:
                    x, y = coords[-1]
                    found[int(m.group(1))] = (int(x), int(y))
        if found:
            self.labels = found
            log(f"learned {len(found)} labels")


def main() -> None:
    argv = sys.argv[1:]
    if "--" in argv:
        argv = argv[argv.index("--") + 1:]
    if not argv:
        print("usage: proxy.py -- <mcp server command>", file=sys.stderr)
        sys.exit(2)
    proxy = Proxy(argv)
    threading.Thread(target=proxy.child_loop, daemon=True).start()
    proxy.client_loop()
    # stdin closed: give the child a moment, then exit
    try:
        proxy.child.wait(5)
    except subprocess.TimeoutExpired:
        proxy.child.kill()


if __name__ == "__main__":
    main()
