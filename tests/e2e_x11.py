"""The crossing and the clipboard on X11, through two real agents, the real
core and real X servers (Xvfb), with a stand-in user and application on
each (tests/fakedesk xdesk) and a real GTK application for the clipboard:

  xdesk A  --XTest-->  X server A  --barrier, raw events-->  agent A  --QUIC-->  agent B  --XTest-->  X server B  -->  xdesk B
  (the user's mouse)                                         (drives B)          (plays it)                           (the application)

Then a third desktop at 200 % (Xft.dpi 192), for points against pixels.

Usage: e2e_x11.py <agent binary> <xdesk binary>.
Needs Xvfb, xrdb, dbus-daemon, and python3-gi with GTK 4 for the clipboard.
RINFLOW_TEST_WM names a window manager to run on every server, for a
desktop closer to a real one: RINFLOW_TEST_WM="mutter --x11" (a compositing
one, which has grabs and barriers of its own)."""
import atexit, json, os, struct, subprocess, sys, time, zlib
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps
from fakedeskclient import session_bus

AGENT, XDESK = sys.argv[1], sys.argv[2]
HERE = os.path.dirname(os.path.abspath(__file__))
step = Steps()
TEXT = "héllo 你好 from X11"
HTML = "<p>Some <b>bold</b> HTML</p>"

# Everything started here, stopped when the test ends however it ends: an
# agent left running would hold its port against the next run.
STARTED = []


@atexit.register
def stop_everything():
    for process in STARTED:
        if process.poll() is None:
            process.kill()


class XServer:
    """Xvfb on a display number it picks itself."""
    def __init__(self, name, size, dpi=None):
        read, write = os.pipe()
        self.log = open(f"/tmp/{name}-xvfb.log", "w")
        # -noreset: a server forgets everything (the resources set below
        # among it) when its last client leaves, unless told not to.
        self.proc = subprocess.Popen(["Xvfb", "-displayfd", str(write), "-screen", "0", f"{size}x24", "-nolisten", "tcp", "-noreset"],
                                     pass_fds=[write], stdout=self.log, stderr=subprocess.STDOUT)
        STARTED.append(self.proc)
        os.close(write)
        number = b""
        while not number.endswith(b"\n"):
            chunk = os.read(read, 16)
            if not chunk:
                raise RuntimeError(f"{name}: Xvfb did not start (see /tmp/{name}-xvfb.log)")
            number += chunk
        os.close(read)
        self.display = f":{number.decode().strip()}"
        manager = os.environ.get("RINFLOW_TEST_WM")
        if manager:
            self.manager_log = open(f"/tmp/{name}-wm.log", "w")
            STARTED.append(subprocess.Popen(manager.split(), env=self.env(DBUS_SESSION_BUS_ADDRESS=session_bus()),
                                            stdout=self.manager_log, stderr=subprocess.STDOUT))
            time.sleep(3)
        if dpi:
            subprocess.run(["xrdb", "-merge"], input=f"Xft.dpi: {dpi}\n".encode(), env=self.env(), check=True)

    def env(self, **more):
        env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "XAUTHORITY", "XDG_SESSION_TYPE", "LIBEI_SOCKET")}
        env.update(DISPLAY=self.display, GDK_BACKEND="x11", **more)
        return env

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.log.close()


class Desk:
    """xdesk on one X server: the user's hands and the application's eyes."""
    def __init__(self, server, window=True):
        self.proc = subprocess.Popen([XDESK] + ([] if window else ["--no-window"]), env=server.env(), stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        STARTED.append(self.proc)

    def cmd(self, **kw):
        self.proc.stdin.write(json.dumps(kw).encode() + b"\n")
        self.proc.stdin.flush()
        return json.loads(self.proc.stdout.readline())

    def push(self, dx, dy, times=1, pause=0.03):
        for _ in range(times):
            self.cmd(cmd="move", dx=dx, dy=dy)
            time.sleep(pause)

    def warp(self, x, y):
        self.cmd(cmd="warp", x=x, y=y)
        time.sleep(0.1)

    def pointer(self):
        at = self.cmd(cmd="pointer")
        return at["x"], at["y"]

    def tap(self, code):
        self.cmd(cmd="key", code=code, down=True)
        self.cmd(cmd="key", code=code, down=False)

    def click(self, button):
        self.cmd(cmd="button", button=button, down=True)
        self.cmd(cmd="button", button=button, down=False)

    def events(self, kind=None):
        events = self.cmd(cmd="events")["events"]
        return [e for e in events if kind is None or e["type"] == kind]

    def clear(self):
        self.cmd(cmd="clear")

    def stop(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=3)


class App:
    """clipapp.py (GTK 4) on one X server."""
    def __init__(self, server, name, bus):
        import socket
        self.path = f"/tmp/{name}-clipapp.sock"
        self.log = open(f"/tmp/{name}-clipapp.log", "w")
        self.proc = subprocess.Popen(["python3", f"{HERE}/clipapp.py", self.path], env=server.env(DBUS_SESSION_BUS_ADDRESS=bus, NO_AT_BRIDGE="1"),
                                     stdout=self.log, stderr=subprocess.STDOUT)
        STARTED.append(self.proc)
        for _ in range(100):
            try:
                self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.s.connect(self.path)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise RuntimeError(f"{name}: clipapp never came up (see /tmp/{name}-clipapp.log)")
        self.f = self.s.makefile("rwb", buffering=0)

    def ask(self, **kw):
        self.s.sendall(json.dumps(kw).encode() + b"\n")
        return json.loads(self.f.readline())

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.log.close()


def wait(predicate, seconds=10):
    end = time.time() + seconds
    while time.time() < end:
        try:
            if predicate():
                return True
        except (KeyError, TypeError):
            pass
        time.sleep(0.2)
    return False


def png(width, height, rows):
    """A PNG whose pixels the test knows: `rows(y)` gives one row of RGBA."""
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    raw = b"".join(b"\x00" + rows(y) for y in range(height))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 1)) + chunk(b"IEND", b""))


def agent_on(name, server, bus, port):
    agent = Agent(name, f"/tmp/home-{name}", f"/tmp/run-{name}", AGENT, port=port,
                  env={"DISPLAY": server.display, "WAYLAND_DISPLAY": None, "XAUTHORITY": None, "XDG_SESSION_TYPE": "x11",
                       "LIBEI_SOCKET": None, "DBUS_SESSION_BUS_ADDRESS": bus})
    STARTED.append(agent.proc)
    return agent


def ready(state):
    return state["input"]["capture"]["state"] == "ready" and state["input"]["emulation"]["state"] == "ready"


def pair(a, b):
    ida, idb = a.state["device_id"], b.state["device_id"]
    found = a.wait(lambda s: any(p["id"] == idb for p in s["peers"])) and b.wait(lambda s: any(p["id"] == ida for p in s["peers"]))
    a.send({"type": "connect", "peer_id": idb})
    if a.wait(lambda s: s.get("pairing"), 15) and b.wait(lambda s: s.get("pairing"), 15):
        a.send({"type": "accept_pairing", "peer_id": idb}); b.send({"type": "accept_pairing", "peer_id": ida})
    return found and a.wait(lambda s: any(x["peer_id"] == idb and x["status"] == "connected" for x in s["sessions"])) and \
        b.wait(lambda s: any(x["peer_id"] == ida and x["status"] == "connected" for x in s["sessions"]))


if __name__ == "__main__":
    xa, xb = XServer("x11a", "1920x1080"), XServer("x11b", "1600x900")
    bus_a, bus_b = session_bus(), session_bus()
    ua, ub = Desk(xa), Desk(xb)
    a, b = agent_on("x11a", xa, bus_a, 42611), agent_on("x11b", xb, bus_b, 42631)
    ida, idb = a.state["device_id"], b.state["device_id"]

    step("both agents reach 'ready' on capture and emulation, with nothing to allow", a.wait(ready, 20) and b.wait(ready, 20),
         f"A={a.state['input']} B={b.state['input']}")
    step("... through the X server", a.state["input"]["capture"]["backend"] == "x11" and b.state["input"]["emulation"]["backend"] == "x11",
         json.dumps(a.state["input"]))
    step("the monitors became the agents' displays", a.state["layout"]["displays"] == [{"x": 0.0, "y": 0.0, "width": 1920.0, "height": 1080.0, "scale": 1.0}]
         and b.state["layout"]["displays"][0]["width"] == 1600.0, json.dumps(a.state["layout"]["displays"]))
    step("the clipboard is there without a permission", a.wait(lambda s: s["input"]["clipboard"], 10) and b.wait(lambda s: s["input"]["clipboard"], 10))

    step("discovered, paired and connected", pair(a, b))
    a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
    step("B placed right of A", a.wait(lambda s: any(d["peer_id"] == idb and d.get("offset") and d["offset"]["x"] == 1920.0 for d in s["layout"]["devices"]), 10))
    offset_b = next(d["offset"] for d in a.state["layout"]["devices"] if d["peer_id"] == idb)
    time.sleep(1.5)  # the barriers follow the placement

    # Before anything: the application on A gets what the user types.
    ua.warp(900, 500); ua.tap(38)
    step("at home, a key goes to the application on A", [(e["code"], e["down"]) for e in ua.events("key")] == [(38, True), (38, False)], json.dumps(ua.events()))

    # An edge no device is placed against is an edge.
    ua.warp(900, 20); ua.push(0, -30, 4)
    step("the top edge, with nothing beyond it, just stops the pointer", ua.pointer() == (900, 0) and a.state.get("driving") is None, str(ua.pointer()))

    def cross(y=500, agent=a, desk=ua, peer=idb, x=1900, dx=40):
        """One push from just inside the edge: enough to hit it, and
        nothing left over to move the other pointer with."""
        desk.warp(x, y)
        desk.push(dx, 0)
        return agent.wait(lambda s: s.get("driving") == peer, 10)

    def back(agent=a, desk=ua, dx=-1000):
        """One move, longer than the way back: what is left of it after
        the edge is dropped."""
        desk.push(dx, 0)
        return agent.wait(lambda s: s.get("driving") is None, 10)

    # The crossing.
    ua.clear(); ub.clear(); ub.warp(800, 450)
    step("pushing the mouse against A's right edge: A drives B", cross(500), repr(a.state.get("driving")))
    step("A's pointer is kept where it hit the edge", ua.pointer() == (1919, 500), str(ua.pointer()))
    entry_y = 500 - offset_b["y"]
    step("B's pointer entered just inside its left edge, at the same height in B's space",
         wait(lambda: 0 <= ub.pointer()[0] <= 4 and abs(ub.pointer()[1] - entry_y) <= 1, 5), f"expected y={entry_y}: {ub.pointer()}")
    entry = ub.pointer()
    step("B's window shows A controlling it", b.wait(lambda s: any(x["peer_id"] == ida and x["remote_control"] for x in s["sessions"]), 10))
    ua.push(100, 0); ua.push(0, 50)
    step("the mouse on A moves B's pointer, as far", wait(lambda: ub.pointer() == (entry[0] + 100, entry[1] + 50), 5), f"from {entry}: {ub.pointer()}")
    step("... while A's stays where it was", ua.pointer() == (1919, 500), str(ua.pointer()))
    ua.tap(38)
    step("a key typed on A arrives in the application on B, the same key",
         wait(lambda: [(e["code"], e["down"]) for e in ub.events("key")] == [(38, True), (38, False)], 5), json.dumps(ub.events()))
    step("... and not in the application on A", ua.events("key") == [], json.dumps(ua.events()))
    step("the application on A was told the keyboard is elsewhere", {"type": "focus", "in": False, "grab": True} in ua.events("focus"), json.dumps(ua.events()))
    ua.click(1); ua.click(3)
    step("left and right clicks arrive on B as left and right, where B's pointer is",
         wait(lambda: [(e["button"], e["down"]) for e in ub.events("button")] == [(1, True), (1, False), (3, True), (3, False)], 5)
         and ub.events("button")[0]["x"] == entry[0] + 100, json.dumps(ub.events("button")))
    step("... and not on A", ua.events("button") == [], json.dumps(ua.events()))
    ub.clear(); ua.click(5); ua.click(5); ua.click(4)
    step("the wheel arrives on B notch for notch, the same way round",
         wait(lambda: [e["button"] for e in ub.events("button") if e["down"]] == [5, 5, 4], 5), json.dumps(ub.events("button")))

    # Back across the edge.
    ua.clear(); ub.clear()
    step("moving back past B's left edge: A no longer drives B", back())
    step("A's pointer is back just inside its right edge, at the height B's pointer left at",
         wait(lambda: 1900 <= ua.pointer()[0] <= 1919 and abs(ua.pointer()[1] - 550) <= 1, 5), str(ua.pointer()))
    step("B's window no longer shows A controlling it", b.wait(lambda s: not any(x["remote_control"] for x in s["sessions"]), 10))
    step("the application on A has the keyboard again", wait(lambda: {"type": "focus", "in": True, "grab": True} in ua.events("focus"), 5), json.dumps(ua.events()))
    ua.clear(); ua.warp(900, 500); ua.tap(40)
    step("at home again, keys go to A and not to B", [(e["code"], e["down"]) for e in ua.events("key")] == [(40, True), (40, False)] and ub.events("key") == [],
         json.dumps([ua.events(), ub.events()]))

    # Ctrl+Alt+Esc (key codes: the evdev codes 29, 56 and 1, plus eight).
    step("second crossing", cross(300))
    for code in (37, 64, 9):
        ua.cmd(cmd="key", code=code, down=True)
    home = a.wait(lambda s: s.get("driving") is None, 10)
    for code in (9, 64, 37):
        ua.cmd(cmd="key", code=code, down=False)
    step("Ctrl+Alt+Esc brings the pointer home", home and 1900 <= ua.pointer()[0] <= 1919, str(ua.pointer()))

    # A menu is open: the application holds the pointer, and nothing crosses.
    time.sleep(0.5)
    step("the application on A opens a menu (it grabs the pointer)", ua.cmd(cmd="grab", on=True)["ok"])
    ua.warp(1900, 400); ua.push(40, 0, 6)
    step("... and pushing against the edge does not cross", not a.wait(lambda s: s.get("driving") == idb, 2))
    ua.cmd(cmd="grab", on=False)
    step("the menu closed: the next push crosses", cross(400))
    step("and back", back())

    # The edge delay: a bump is not a crossing, and does not take the
    # keyboard from the application; a push is one.
    a.send({"type": "update_settings", "values": {"switch_delay_ms": 600}})
    step("a delay before crossing is set", a.wait(lambda s: s["settings"]["switch_delay_ms"] == 600, 5))
    time.sleep(0.5); ua.clear()
    ua.warp(1890, 600); ua.push(40, 0, 1)
    time.sleep(1.2)
    step("a bump against the edge does not cross", a.state.get("driving") is None and a.wait(lambda s: s.get("driving") is None, 1))
    step("... and the application on A kept the keyboard throughout", ua.events("focus") == [], json.dumps(ua.events()))
    ua.tap(38)
    step("... and its keys", [(e["code"], e["down"]) for e in ua.events("key")] == [(38, True), (38, False)], json.dumps(ua.events()))
    ua.warp(1890, 600); ua.push(10, 0, 40, 0.03)
    step("pushing on for longer than the delay crosses", a.wait(lambda s: s.get("driving") == idb, 5))
    step("and back", back())
    a.send({"type": "update_settings", "values": {"switch_delay_ms": 0}})
    a.wait(lambda s: s["settings"]["switch_delay_ms"] == 0, 5)

    # The other way: B's user drives A.
    time.sleep(0.5); ua.clear(); ua.warp(900, 500)
    step("pushing against B's left edge: B drives A", cross(400, agent=b, desk=ub, peer=ida, x=20, dx=-40), repr(b.state.get("driving")))
    step("A's pointer entered just inside its right edge", wait(lambda: 1915 <= ua.pointer()[0] <= 1919 and abs(ua.pointer()[1] - (400 + offset_b["y"])) <= 1, 5), str(ua.pointer()))
    ub.tap(39)
    step("a key typed on B arrives in the application on A", wait(lambda: [(e["code"], e["down"]) for e in ua.events("key")] == [(39, True), (39, False)], 5), json.dumps(ua.events()))
    step("and back to B", back(agent=b, desk=ub, dx=1000))

    # The other two edges: B below A.
    a.send({"type": "place_beside", "peer_id": idb, "side": "bottom"})
    step("B placed below A", a.wait(lambda s: any(d["peer_id"] == idb and d.get("offset") and d["offset"]["y"] == 1080.0 for d in s["layout"]["devices"]), 10))
    below = next(d["offset"] for d in a.state["layout"]["devices"] if d["peer_id"] == idb)
    time.sleep(1.5); ub.warp(800, 450)
    ua.warp(700, 1060); ua.push(0, 40)
    step("pushing against A's bottom edge: A drives B", a.wait(lambda s: s.get("driving") == idb, 10) and ua.pointer() == (700, 1079), str(ua.pointer()))
    step("B's pointer entered just inside its top edge, at the same place across",
         wait(lambda: abs(ub.pointer()[0] - (700 - below["x"])) <= 1 and 0 <= ub.pointer()[1] <= 4, 5), f"expected x={700 - below['x']}: {ub.pointer()}")
    ua.push(0, -1000)
    step("and back up", a.wait(lambda s: s.get("driving") is None, 10) and wait(lambda: 1060 <= ua.pointer()[1] <= 1079, 5), str(ua.pointer()))
    time.sleep(0.5)
    ub.warp(500, 20); ub.push(0, -40)
    step("pushing against B's top edge: B drives A", b.wait(lambda s: s.get("driving") == ida, 10) and ub.pointer() == (500, 0), str(ub.pointer()))
    ub.push(0, 1000)
    step("and back down", b.wait(lambda s: s.get("driving") is None, 10))
    a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
    step("B placed right of A again", a.wait(lambda s: any(d["peer_id"] == idb and d.get("offset") and d["offset"]["x"] == 1920.0 for d in s["layout"]["devices"]), 10))
    time.sleep(1.5)

    # --- The clipboard, with a real GTK application on each desktop -------
    ua.stop(); ub.stop()   # their windows would cover GTK's
    ua, ub = Desk(xa, window=False), Desk(xb, window=False)
    app_a, app_b = App(xa, "x11a", bus_a), App(xb, "x11b", bus_b)
    step("a GTK application is up on each desktop", "formats" in app_a.ask(formats=True) and "formats" in app_b.ask(formats=True))
    for agent in (a, b):
        agent.send({"type": "update_settings", "values": {"clipboard": True}})
    step("clipboard sharing on, both sides", a.wait(lambda s: s["settings"]["clipboard"]) and b.wait(lambda s: s["settings"]["clipboard"]))
    cross_ab = lambda y: cross(y, desk=ua)
    back_ab = lambda: back(desk=ua)

    # Text, A → B.
    step("GTK on A copies text", app_a.ask(copy_text=TEXT).get("ok") and wait(lambda: app_a.ask(formats=True)["formats"], 5))
    time.sleep(1)
    step("the pointer crosses from A into B", cross_ab(500))
    step("B's clipboard now offers text from Rinflow",
         wait(lambda: "text/plain;charset=utf-8" in app_b.ask(formats=True)["formats"], 10), json.dumps(app_b.ask(formats=True)))
    pasted = app_b.ask(paste_text=True)
    step("GTK on B pastes exactly what was copied on A", pasted.get("text") == TEXT, json.dumps(pasted, ensure_ascii=False))

    # A large image, B → A, when the pointer comes back: larger than one X
    # request carries, so it moves in pieces (INCR), out of GTK and into it.
    def row(y):
        # Noise does not compress; the first pixel is known.
        noise = os.urandom(4 * 1200)
        return bytes((255, 0, 0, 255)) + noise[4:] if y == 0 else noise
    image = png(1200, 1000, row)
    step("the image is larger than GTK or Rinflow puts in one request", len(image) > 4500 * 1024, f"{len(image)} bytes")
    step("GTK on B copies a large image", app_b.ask(copy_png=image.hex()).get("ok") and wait(lambda: "image/png" in app_b.ask(formats=True)["formats"], 5))
    time.sleep(2)
    step("the pointer comes back to A", back_ab())
    step("A's clipboard now offers an image from Rinflow", wait(lambda: "image/png" in app_a.ask(formats=True)["formats"], 10), json.dumps(app_a.ask(formats=True)))
    got = app_a.ask(paste_png=True)
    step("GTK on A pastes B's image whole: same size, same first pixel", got.get("width") == 1200 and got.get("height") == 1000 and got.get("pixel") == [255, 0, 0, 255], json.dumps(got))
    logs = open("/tmp/x11b.log").read(), open("/tmp/x11a.log").read()
    step("it left GTK in pieces and reached GTK in pieces", "image/png comes in pieces" in logs[0] and "in pieces" in logs[1].split("pasting image/png")[-1][:40],
         "needs RINFLOW_TEST_LOG=debug" if os.environ.get("RINFLOW_TEST_LOG") != "debug" else "")

    # HTML with its text fallback, A → B again.
    step("GTK on A copies HTML with a text fallback", app_a.ask(copy_html=HTML, text="Some bold HTML").get("ok") and wait(lambda: "text/html" in app_a.ask(formats=True)["formats"], 5))
    time.sleep(1)
    step("the pointer crosses again", cross_ab(300))
    step("B's clipboard offers the HTML too", wait(lambda: "text/html" in app_b.ask(formats=True)["formats"], 10), json.dumps(app_b.ask(formats=True)))
    step("... and its text", app_b.ask(paste_text=True).get("text") == "Some bold HTML")
    step("and back", back_ab())

    # Sharing off: what is copied here stays here.
    a.send({"type": "update_settings", "values": {"clipboard": False}})
    a.wait(lambda s: not s["settings"]["clipboard"], 5)
    app_a.ask(copy_text="kept at home"); time.sleep(1)
    step("with sharing off on A, a crossing carries nothing", cross_ab(500) and not wait(lambda: app_b.ask(paste_text=True).get("text") == "kept at home", 2),
         json.dumps(app_b.ask(paste_text=True), ensure_ascii=False))
    step("and back", back_ab())
    app_a.stop(); app_b.stop(); ua.stop(); ub.stop()

    # --- The X server going away -----------------------------------------
    xb.stop()
    step("B's X server is gone: its backends wait, the agent stays", b.wait(lambda s: s["input"]["capture"]["state"] == "waiting" and s["input"]["emulation"]["state"] == "waiting", 10)
         and b.proc.poll() is None, json.dumps(b.state["input"]))
    a.quit(); b.quit()
    step("agents exit cleanly", a.proc.returncode == 0 and b.proc.returncode == 0, f"{a.proc.returncode} {b.proc.returncode}")
    xa.stop()

    # --- A desktop at 200 %: points against pixels -------------------------
    xc, xd = XServer("x11c", "3840x2160", dpi=192), XServer("x11d", "1600x900")
    bus_c, bus_d = session_bus(), session_bus()
    uc, ud = Desk(xc), Desk(xd)
    c, d = agent_on("x11c", xc, bus_c, 42651), agent_on("x11d", xd, bus_d, 42671)
    idc, idd = c.state["device_id"], d.state["device_id"]
    step("a 3840×2160 screen at Xft.dpi 192 is a 1920×1080 display at scale 2", c.wait(ready, 20) and d.wait(ready, 20)
         and c.state["layout"]["displays"] == [{"x": 0.0, "y": 0.0, "width": 1920.0, "height": 1080.0, "scale": 2.0}], json.dumps(c.state["layout"]["displays"]))
    step("paired and connected", pair(c, d))
    c.send({"type": "place_beside", "peer_id": idd, "side": "right"})
    step("the 1600×900 desktop placed right of it", c.wait(lambda s: any(x["peer_id"] == idd and x.get("offset") and x["offset"]["x"] == 1920.0 for x in s["layout"]["devices"]), 10),
         json.dumps(c.state["layout"]["devices"]))
    offset_d = next(x["offset"] for x in c.state["layout"]["devices"] if x["peer_id"] == idd)
    time.sleep(1.5); ud.warp(800, 450)
    uc.warp(3800, 1000); uc.push(80, 0)
    step("pushing against its right edge, at pixel 3839: it drives the other", c.wait(lambda s: s.get("driving") == idd, 10) and uc.pointer() == (3839, 1000), str(uc.pointer()))
    step("pixel row 1000 is point 500: the other pointer enters at that height",
         wait(lambda: 0 <= ud.pointer()[0] <= 4 and abs(ud.pointer()[1] - (500 - offset_d["y"])) <= 1, 5), f"expected y={500 - offset_d['y']}: {ud.pointer()}")
    entry = ud.pointer()
    uc.push(200, 0); uc.push(0, 100)
    step("200 pixels of mouse there are 100 points here", wait(lambda: ud.pointer() == (entry[0] + 100, entry[1] + 50), 5), f"from {entry}: {ud.pointer()}")
    uc.push(-2000, 0)
    step("back: the pointer returns at twice the points, in pixels", c.wait(lambda s: s.get("driving") is None, 10)
         and wait(lambda: 3800 <= uc.pointer()[0] <= 3839 and abs(uc.pointer()[1] - 2 * (entry[1] + 50 + offset_d["y"])) <= 2, 5), str(uc.pointer()))
    # The other way: points become pixels when played.
    ud.warp(20, 300); ud.push(-40, 0)
    step("the other desktop drives this one", d.wait(lambda s: s.get("driving") == idc, 10))
    step("its pointer enters at twice the points, in pixels", wait(lambda: 3820 <= uc.pointer()[0] <= 3839 and abs(uc.pointer()[1] - 2 * (300 + offset_d["y"])) <= 2, 5), str(uc.pointer()))
    before = uc.pointer()
    ud.push(-30, 0); ud.push(0, 25)
    step("30 points of mouse there are 60 pixels here", wait(lambda: uc.pointer() == (before[0] - 60, before[1] + 50), 5), f"from {before}: {uc.pointer()}")
    ud.push(2000, 0)
    step("and back", d.wait(lambda s: s.get("driving") is None, 10))
    uc.stop(); ud.stop()
    c.quit(); d.quit()
    step("agents exit cleanly", c.proc.returncode == 0 and d.proc.returncode == 0, f"{c.proc.returncode} {d.proc.returncode}")
    xc.stop(); xd.stop()
    sys.exit(step.finish())
