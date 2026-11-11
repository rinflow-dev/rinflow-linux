"""The arrangement canvas, driven with a real pointer: a stand-in agent on
the window's socket serves an arrangement (this device, a placed Mac, an
unplaced laptop on the strip), answers previews with a simple snap, and
records what the window asks. Checks that a click places nothing, that a
chip dragged beside the screen gets placed, that a placed device dragged to
the strip is unplaced, that Escape puts a dragged device back, and that
previews are asked one at a time.

Run inside an X display with a session bus (the Makefile's e2e-canvas uses
xvfb-run and dbus-run-session): e2e_canvas.py <window> <agent> [screenshots]"""
import copy, json, os, shutil, socket, subprocess, sys, tempfile, threading, time
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps

SHOTS = None
step = Steps()
LOCAL = {"x": 0, "y": 0, "width": 1920, "height": 1080, "scale": 1}
MAC = [{"x": 0, "y": 0, "width": 1512, "height": 982, "scale": 2}]
LAPTOP = [{"x": 0, "y": 0, "width": 1440, "height": 900, "scale": 1}]


def template(AGENT):
    """A real snapshot from a real agent, to fill in."""
    root = tempfile.mkdtemp(prefix="rinflow-canvas-template-")
    agent = Agent("canvas-template", f"{root}/home", f"{root}/run", AGENT, port=42561,
                  env={"WAYLAND_DISPLAY": None, "DISPLAY": None, "DBUS_SESSION_BUS_ADDRESS": "unix:path=/nonexistent"})
    agent.wait(lambda s: s is not None, 10)
    state = copy.deepcopy(agent.state)
    agent.quit()
    shutil.rmtree(root, ignore_errors=True)
    return state


def snap(peer, x, y):
    """Against the nearest side of this device's screen, overlapping it."""
    w, h = peer[0]["width"], peer[0]["height"]
    cx, cy = x + w / 2, y + h / 2
    dx, dy = cx - 960, cy - 540
    if abs(dx) / 1920 >= abs(dy) / 1080:
        return {"x": 1920 if dx > 0 else -w, "y": min(max(y, -h + 200), 1080 - 200)}
    return {"x": min(max(x, -w + 200), 1920 - 200), "y": 1080 if dy > 0 else -h}


class FakeAgent:
    def __init__(self, path, state, on_request=None):
        self.state = state
        # Answers what handle() doesn't (it runs first, with the request).
        self.on_request = on_request
        self.requests = []
        self.previews_in_flight = 0
        self.most_in_flight = 0
        self.lock = threading.Lock()
        os.makedirs(os.path.dirname(path), exist_ok=True)
        self.server = socket.socket(socket.AF_UNIX)
        self.server.bind(path)
        self.server.listen(4)
        self.conn = None
        threading.Thread(target=self.serve, daemon=True).start()

    def send(self, obj):
        if self.conn:
            self.conn.sendall((json.dumps(obj) + "\n").encode())

    def device(self, peer_id):
        return next(d for d in self.state["layout"]["devices"] if d["peer_id"] == peer_id)

    def serve(self):
        self.conn, _ = self.server.accept()
        self.send({"type": "hello", "protocol": 3, "agent_version": self.state["agent_version"]})
        self.send(dict(self.state, type="state"))
        buf = b""
        while True:
            chunk = self.conn.recv(65536)
            if not chunk:
                return
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                self.handle(json.loads(line))

    def handle(self, request):
        with self.lock:
            self.requests.append(request)
        if self.on_request:
            self.on_request(self, request)
        kind = request["type"]
        if kind == "preview_placement":
            with self.lock:
                self.previews_in_flight += 1
                self.most_in_flight = max(self.most_in_flight, self.previews_in_flight)
            device = self.device(request["peer_id"])
            offset = snap(device["displays"], request["x"], request["y"])

            def answer():
                time.sleep(0.03)  # a little latency, as over a socket
                with self.lock:
                    self.previews_in_flight -= 1
                self.send({"type": "placement_preview", "peer_id": request["peer_id"], "offset": offset})
            threading.Thread(target=answer, daemon=True).start()
        elif kind == "set_placement":
            device = self.device(request["peer_id"])
            device["offset"] = snap(device["displays"], request["x"], request["y"])
            self.send(dict(self.state, type="state"))
        elif kind == "remove_placement":
            self.device(request["peer_id"])["offset"] = None
            self.send(dict(self.state, type="state"))
        elif kind == "quit":
            self.state["quitting"] = True
            self.send(dict(self.state, type="state"))

    def taken(self, kind):
        with self.lock:
            found = [r for r in self.requests if r["type"] == kind]
            self.requests = [r for r in self.requests if r["type"] != kind]
            return found


def find_canvas():
    def walk(node, depth=0):
        if node is None or depth > 60:
            return None
        try:
            if node.get_role() == Atspi.Role.IMAGE and (node.get_name() or "").startswith("Arrangement"):
                return node
            for i in range(node.get_child_count()):
                found = walk(node.get_child_at_index(i), depth + 1)
                if found:
                    return found
        except Exception:
            return None
        return None
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        found = walk(desktop.get_child_at_index(i))
        if found:
            return found
    return None


def screenshot(name):
    if SHOTS:
        os.makedirs(SHOTS, exist_ok=True)
        subprocess.run(["import", "-window", "root", os.path.join(SHOTS, f"{name}.png")], check=False)


class Pointer:
    """Real pointer events (XTest through AT-SPI), in canvas pixels."""
    def __init__(self, origin):
        self.origin = origin

    def at(self, x, y):
        return int(self.origin[0] + x), int(self.origin[1] + y)

    def move(self, x, y):
        Atspi.generate_mouse_event(*self.at(x, y), "abs")
        time.sleep(0.02)

    def drag(self, start, end, steps=25, before_release=None):
        self.move(*start)
        Atspi.generate_mouse_event(*self.at(*start), "b1p")
        time.sleep(0.05)
        for i in range(1, steps + 1):
            self.move(start[0] + (end[0] - start[0]) * i / steps, start[1] + (end[1] - start[1]) * i / steps)
        time.sleep(0.25)
        if before_release:
            before_release()
        Atspi.generate_mouse_event(*self.at(*end), "b1r")
        time.sleep(0.6)


if __name__ == "__main__":
    WINDOW = os.path.abspath(sys.argv[1])
    AGENT = os.path.abspath(sys.argv[2])
    SHOTS = sys.argv[3] if len(sys.argv) > 3 else None
    state = template(AGENT)
    state["device_name"] = "Workstation"
    state["layout"] = {"displays": [LOCAL], "devices": [
        {"peer_id": "mac", "name": "MacBook", "platform": "macos", "displays": MAC, "offset": {"x": 1920, "y": 0},
         "side": "right", "remembered": True, "last_seen_ms": None},
        {"peer_id": "laptop", "name": "ThinkPad", "platform": "linux", "displays": LAPTOP, "offset": None,
         "side": None, "remembered": True, "last_seen_ms": None},
    ]}
    state["peers"] = [{"id": p, "name": n, "platform": pl, "address": "192.0.2.1", "port": 42421}
                      for p, n, pl in [("mac", "MacBook", "macos"), ("laptop", "ThinkPad", "linux")]]
    state["sessions"] = [{"peer_id": "mac", "name": "MacBook", "status": "connected", "remote_control": False,
                          "latency_ms": 3, "paired": True}]

    root = tempfile.mkdtemp(prefix="rinflow-canvas-")
    home, runtime = os.path.join(root, "home"), os.path.join(root, "run")
    os.makedirs(home)
    os.makedirs(runtime, mode=0o700)
    fake = FakeAgent(os.path.join(runtime, "rinflow", "agent.sock"), state)
    env = dict(os.environ, HOME=home, XDG_RUNTIME_DIR=runtime, XDG_CONFIG_HOME=os.path.join(home, ".config"),
               GDK_BACKEND="x11", LANG="C.UTF-8", GTK_A11Y="atspi", RUST_LOG="info")
    for name in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "WAYLAND_DISPLAY"):
        env.pop(name, None)
    window = subprocess.Popen([WINDOW], env=env)
    try:
        canvas = None
        deadline = time.time() + 30
        while time.time() < deadline and canvas is None:
            canvas = find_canvas()
            time.sleep(0.3)
        step("the canvas is there, named for screen readers", canvas is not None)
        if canvas is None:
            sys.exit(step.finish())
        time.sleep(1.0)
        box = canvas.get_extents(Atspi.CoordType.WINDOW)
        width, height = box.width, box.height
        pointer = Pointer((box.x, box.y))
        step("its name lists the placed devices", "MacBook" in canvas.get_name(), canvas.get_name())
        screenshot("0-start")

        # Where things are drawn: the same fit as canvas.rs.
        pad_side, pad_top, pad_bottom, strip = 24, 14, 58, 44
        world = (-1920 * 0.6, -1080 * 0.35, 1920 * 2.2, 1080 * 1.7)
        world = (world[0], world[1], max(world[0] + world[2], 1920 + 1512) - world[0], world[3])
        aw, ah = width - 2 * pad_side, height - pad_top - pad_bottom
        scale = min(aw / world[2], ah / world[3])
        left = world[0] - (aw / scale - world[2]) / 2 - pad_side / scale
        top = world[1] - (ah / scale - world[3]) / 2 - pad_top / scale
        px = lambda x, y: ((x - left) * scale, (y - top) * scale)
        mac_centre = px(1920 + 756, 491)

        # A click is not a drag.
        pointer.move(*mac_centre)
        Atspi.generate_mouse_event(*pointer.at(*mac_centre), "b1c")
        time.sleep(0.5)
        step("a click places nothing", not fake.taken("set_placement") and not fake.taken("remove_placement"))

        # Escape while dragging puts it back.
        def escape():
            screenshot("1-dragging-mac")
            Atspi.generate_keyboard_event(0xff1b, None, Atspi.KeySynthType.SYM)
            time.sleep(0.4)
        pointer.drag(mac_centre, px(960, 1500), before_release=escape)
        step("Escape puts a dragged device back", not fake.taken("set_placement") and not fake.taken("remove_placement"))
        previews = fake.taken("preview_placement")
        step("previews were asked while dragging", len(previews) > 0, f"{len(previews)}")
        step("... one at a time", fake.most_in_flight <= 1, f"at most {fake.most_in_flight} in flight")

        # A chip from the strip, beside the left edge of the screen.
        chip = (24 / 2 + 60, height - strip / 2)
        target = px(-720, 540)
        pointer.drag(chip, target, before_release=lambda: screenshot("2-dragging-chip"))
        placed = fake.taken("set_placement")
        step("a chip dragged beside the screen is placed", len(placed) == 1 and placed[0]["peer_id"] == "laptop",
             str(placed or [r["type"] for r in fake.requests]))
        step("... on the left", placed and fake.device("laptop")["offset"]["x"] < 0, str(fake.device("laptop")["offset"]))
        time.sleep(0.6)
        screenshot("3-chip-placed")

        # The view has re-fit for the laptop.
        world = (min(-1920 * 0.6, -1440), -1080 * 0.35, 0, 1080 * 1.7)
        world = (world[0], world[1], max(1920 * 1.6, 1920 + 1512) - world[0], world[3])
        scale = min(aw / world[2], ah / world[3])
        left = world[0] - (aw / scale - world[2]) / 2 - pad_side / scale
        top = world[1] - (ah / scale - world[3]) / 2 - pad_top / scale
        mac_centre = px(1920 + 756, 491)

        # Far from where it would land, an outline shows the spot; Escape
        # puts it back.
        laptop = fake.device("laptop")["offset"]
        laptop_centre = px(laptop["x"] + 720, laptop["y"] + 450)
        pointer.drag(laptop_centre, (laptop_centre[0] - 50, laptop_centre[1] + 20),
                     before_release=lambda: (screenshot("3b-outline"), escape()))
        step("Escape after a long drag places nothing", not fake.taken("set_placement"))
        fake.taken("preview_placement")

        # The Mac to the strip: unplaced.
        pointer.drag(mac_centre, (width / 2, height - strip / 2), before_release=lambda: screenshot("4-over-strip"))
        removed = fake.taken("remove_placement")
        step("a placed device dropped on the strip is unplaced", [r["peer_id"] for r in removed] == ["mac"], str(removed))
        step("... and not placed", not fake.taken("set_placement"))
        time.sleep(0.6)
        screenshot("5-mac-unplaced")
        step("the window is still running", window.poll() is None)
    finally:
        fake.send(dict(fake.state, type="state", quitting=True))
        try:
            window.wait(10)
        except subprocess.TimeoutExpired:
            window.kill()
        shutil.rmtree(root, ignore_errors=True)
    sys.exit(step.finish())
