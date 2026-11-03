"""The crossing on two real GNOME compositors: Mutter headless with the GNOME
portal backend on each side (GNOME 46 on Ubuntu 24.04 in the container).
A's pointer is moved by an injector through Mutter A's own RemoteDesktop/EIS
path; what Mutter B delivers is recorded by a full-screen GTK window on B.

  injector ─RemoteDesktop/EIS→ Mutter A ─InputCapture/EIS→ agent A ─QUIC→ agent B ─RemoteDesktop/EIS→ Mutter B → observer window

Usage (in the build container): e2e_gnome.py <agent> <injector> ; needs mutter,
xdg-desktop-portal(-gnome), dbus, at-spi2-core, python3-gi, gir1.2-gtk-4.0."""
import json, os, shutil, socket, subprocess, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from agentclient import Agent, Steps

step = Steps()


DESKTOP = os.environ.get("RINFLOW_TEST_DESKTOP", "gnome")  # or "kde"


class Stack:
    """One desktop: session bus, a headless compositor and its portal backend.
    GNOME (Mutter + xdg-desktop-portal-gnome) by default; KDE (KWin +
    xdg-desktop-portal-kde) with RINFLOW_TEST_DESKTOP=kde."""

    def __init__(self, name, wayland, size="1920x1080"):
        self.name = name
        self.kind = DESKTOP
        self.runtime = f"/tmp/gn-{name}/run"
        self.home = f"/tmp/gn-{name}/home"
        # Leftovers of an earlier run with this name: its bus and whatever
        # the bus activated keep writing into the runtime directory.
        subprocess.run(["pkill", "-f", f"address=unix:path={self.runtime}/bus"], capture_output=True)
        time.sleep(0.5)
        for _ in range(5):
            shutil.rmtree(f"/tmp/gn-{name}", ignore_errors=True)
            if not os.path.exists(f"/tmp/gn-{name}"):
                break
            time.sleep(0.5)
        os.makedirs(self.runtime, mode=0o700, exist_ok=True); os.makedirs(self.home, exist_ok=True)
        # The bus is started with the desktop's own environment: services it
        # activates (the accessibility bus among them) inherit it, so the two
        # desktops never share one.
        self.env = dict(os.environ, XDG_RUNTIME_DIR=self.runtime, HOME=self.home,
                        XDG_SESSION_TYPE="wayland", XDG_CURRENT_DESKTOP="GNOME" if self.kind == "gnome" else "KDE",
                        WAYLAND_DISPLAY=wayland, GSK_RENDERER="cairo", LANG="en_US.UTF-8",
                        RUST_LOG=os.environ.get("RINFLOW_TEST_RUST_LOG", "info"))
        if self.kind == "kde":
            # Qt: software rendering, and the accessibility bridge on so the
            # portal dialogs can be pressed.
            self.env.update(QT_QPA_PLATFORM="wayland", LIBGL_ALWAYS_SOFTWARE="1",
                            QT_LINUX_ACCESSIBILITY_ALWAYS_ON="1", KWIN_COMPOSE="Q")
        self.env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        self.bus = subprocess.check_output(["dbus-daemon", "--session", "--print-address", "--fork",
                                            f"--address=unix:path={self.runtime}/bus"], env=self.env).decode().strip()
        self.env["DBUS_SESSION_BUS_ADDRESS"] = self.bus
        self.procs = []
        if self.kind == "gnome":
            self.spawn(["mutter", "--headless", "--wayland", "--no-x11", "--virtual-monitor", size, "--wayland-display", wayland], "mutter")
        else:
            width, height = size.split("x")
            self.spawn(["kwin_wayland", "--virtual", "--no-lockscreen", "--no-global-shortcuts",
                        "--width", width, "--height", height, "--socket", wayland], "kwin")
        for _ in range(100):
            if os.path.exists(f"{self.runtime}/{wayland}"):
                break
            time.sleep(0.2)
        time.sleep(1 if self.kind == "gnome" else 4)
        self.spawn(["/usr/libexec/xdg-desktop-portal-gnome" if self.kind == "gnome" else "/usr/libexec/xdg-desktop-portal-kde"], "xdpg")
        time.sleep(1.5 if self.kind == "gnome" else 3)
        self.spawn(["/usr/libexec/xdg-desktop-portal", "--verbose"], "xdp")
        time.sleep(2.5)

    def spawn(self, argv, tag, env=None):
        log = open(f"/tmp/gn-{self.name}/{tag}.log", "w")
        proc = subprocess.Popen(argv, env=dict(self.env, **(env or {})), stdout=log, stderr=subprocess.STDOUT)
        self.procs.append(proc)
        return proc

    def allow(self, seconds=20):
        """Presses the affirmative button of every portal dialog on this desktop."""
        return subprocess.run(["python3", f"{HERE}/a11y_allow.py", str(seconds)], env=self.env,
                              capture_output=True, text=True).stdout.strip()

    def allow_until(self, agent, predicate, seconds=40):
        """Keeps pressing dialogs (they can appear seconds apart: the agent
        starts its two backends independently) until the agent's snapshot
        satisfies `predicate`."""
        end = time.time() + seconds
        pressed = []
        while time.time() < end:
            if agent.wait(predicate, 2):
                break
            out = self.allow(4)
            pressed += [l for l in out.splitlines() if l.startswith("pressed '")]
        return "; ".join(pressed)

    def stop(self):
        for proc in reversed(self.procs):
            proc.terminate()
        for proc in self.procs:
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
        # The bus (forked) and the services it activated.
        subprocess.run(["pkill", "-f", f"address=unix:path={self.runtime}/bus"], capture_output=True)


class Control:
    def __init__(self, path):
        for _ in range(100):
            try:
                self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); self.s.connect(path); break
            except OSError:
                time.sleep(0.1)
        else:
            raise RuntimeError(f"{path} never appeared")
        self.f = self.s.makefile("rwb", buffering=0)

    def cmd(self, **kw):
        self.s.sendall(json.dumps(kw).encode() + b"\n")
        return json.loads(self.f.readline())


def events(path):
    out = []
    try:
        for line in open(path):
            line = line.strip()
            if line.startswith("{"):
                out.append(json.loads(line))
    except FileNotFoundError:
        pass
    return out


def wait(predicate, seconds=15):
    end = time.time() + seconds
    while time.time() < end:
        if predicate():
            return True
        time.sleep(0.2)
    return False


if __name__ == "__main__":
    AGENT, INJECTOR = sys.argv[1], sys.argv[2]
    print(f"bringing up two {DESKTOP.upper()} desktops…", flush=True)
    A = Stack("a", "wayland-9")
    B = Stack("b", "wayland-8", size="1600x900")
    step("the portal offers InputCapture and RemoteDesktop on both desktops", all(
        "org.freedesktop.portal.Desktop" in subprocess.check_output(
            ["dbus-send", "--session", "--print-reply", "--dest=org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus.ListNames"],
            env=s.env).decode() for s in (A, B)))

    a = Agent("ga", A.home, A.runtime, AGENT, port=42441, env=A.env, fresh=False)
    b = Agent("gb", B.home, B.runtime, AGENT, port=42431, env=B.env, fresh=False)
    ida, idb = a.state["device_id"], b.state["device_id"]
    prompting = lambda s: s["input"]["capture"]["state"] == "prompting" or s["input"]["emulation"]["state"] == "prompting"
    step("GNOME asks for permission (agents report 'prompting')", a.wait(prompting, 15) and b.wait(prompting, 15))
    ready = lambda s: s["input"]["capture"]["state"] == "ready" and s["input"]["emulation"]["state"] == "ready"
    print("  dialogs A:", A.allow_until(a, ready), "| dialogs B:", B.allow_until(b, ready), flush=True)
    step("after allowing: capture and emulation 'ready' on both real desktops", a.wait(ready, 5) and b.wait(ready, 5),
         f"A={a.state['input']} B={b.state['input']}")
    step("Mutter's zones became the displays (1920x1080 / 1600x900)",
         a.state["layout"]["displays"] == [{"x": 0.0, "y": 0.0, "width": 1920.0, "height": 1080.0, "scale": 1.0}] and
         b.state["layout"]["displays"] == [{"x": 0.0, "y": 0.0, "width": 1600.0, "height": 900.0, "scale": 1.0}],
         json.dumps(a.state["layout"]["displays"] + b.state["layout"]["displays"]))
    token = os.path.join(B.home, ".local/state/rinflow/remote-desktop.token")
    step("GNOME's RemoteDesktop restore token saved", os.path.exists(token) and len(open(token).read().strip()) > 8)

    # The observer on B, full screen, records what Mutter B delivers.
    B.spawn(["python3", f"{HERE}/observer.py", "/tmp/gn-b/observer.log"], "observer")
    step("observer window up on B", wait(lambda: any(e["type"] == "ready" for e in events("/tmp/gn-b/observer.log")), 15))

    # The injector on A: a virtual mouse and keyboard through Mutter A's own portal.
    A.spawn([INJECTOR, f"{A.runtime}/injector.sock"], "injector")
    time.sleep(2)
    print("  injector dialog A:", A.allow(10), flush=True)
    inj = Control(f"{A.runtime}/injector.sock")
    step("injector connected to Mutter A through RemoteDesktop/EIS", wait(lambda: inj.cmd(state=True).get("ready"), 15))
    # A headless Mutter parks its pointer at (0, 0), on the very line B's own
    # barrier will sit on; a desktop in use has it somewhere in the middle.
    B.spawn([INJECTOR, f"{B.runtime}/injector.sock"], "injector")
    time.sleep(2)
    print("  injector dialog B:", B.allow(10), flush=True)
    inj_b = Control(f"{B.runtime}/injector.sock")
    wait(lambda: inj_b.cmd(state=True).get("ready"), 15)
    inj_b.cmd(abs=[800, 450]); time.sleep(0.3)

    # Pair, connect, place B to the right of A.
    found = a.wait(lambda s: any(p["id"] == idb for p in s["peers"])) and b.wait(lambda s: any(p["id"] == ida for p in s["peers"]))
    step("discovery", found)
    a.send({"type": "connect", "peer_id": idb})
    if a.wait(lambda s: s.get("pairing"), 15) and b.wait(lambda s: s.get("pairing"), 15):
        a.send({"type": "accept_pairing", "peer_id": idb}); b.send({"type": "accept_pairing", "peer_id": ida})
    step("paired and connected", a.wait(lambda s: any(x["peer_id"] == idb and x["status"] == "connected" for x in s["sessions"])) and
         b.wait(lambda s: any(x["peer_id"] == ida and x["status"] == "connected" for x in s["sessions"])))
    a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
    step("B placed right of A", a.wait(lambda s: any(d["peer_id"] == idb and d.get("offset") and d["offset"]["x"] == 1920.0 for d in s["layout"]["devices"]), 10))
    offset_b = next(d["offset"] for d in a.state["layout"]["devices"] if d["peer_id"] == idb)
    time.sleep(2)  # barriers set and enabled on Mutter A
    step("capture still 'ready' after barriers were set on Mutter (SetPointerBarriers accepted, Enable ok)", a.state["input"]["capture"]["state"] == "ready", json.dumps(a.state["input"]["capture"]))

    # The crossing: park the pointer near A's right edge, then push through it.
    inj.cmd(abs=[1800, 500]); time.sleep(0.3)
    for _ in range(6):
        inj.cmd(rel=[40, 0]); time.sleep(0.1)
    step("the pointer hit Mutter's barrier: A drives B", a.wait(lambda s: s.get("driving") == idb, 10), repr(a.state.get("driving")))
    # The pushes that went on after the barrier fired were already forwarded to
    # B, so B's pointer is some multiple of 40 right of the entry point; what
    # must hold exactly is the entry height and every delta from here on.
    time.sleep(0.5)
    arrived = [e for e in events("/tmp/gn-b/observer.log") if e["type"] in ("motion", "enter")]
    step("Mutter B delivered the entry to a real window on B, at the entry height (500 on A = %.0f on B)" % (500 - offset_b["y"]),
         bool(arrived) and abs(arrived[-1]["y"] - (500 - offset_b["y"])) < 1 and arrived[-1]["x"] < 400, json.dumps(arrived[-1:]))
    start = arrived[-1] if arrived else {"x": 0, "y": 0}
    inj.cmd(rel=[60, 0]); time.sleep(0.2); inj.cmd(rel=[0, 40]); time.sleep(0.5)
    moved = [e for e in events("/tmp/gn-b/observer.log") if e["type"] in ("motion", "enter")]
    last = moved[-1] if moved else start
    step("relative motion on A moves the pointer on B by the same amount (+60, +40)",
         abs(last["x"] - start["x"] - 60) < 1 and abs(last["y"] - start["y"] - 40) < 1,
         f"B: ({start['x']:.0f}, {start['y']:.0f}) → ({last['x']:.0f}, {last['y']:.0f})")
    before = len(events("/tmp/gn-b/observer.log"))
    inj.cmd(key=[0x04, True]); time.sleep(0.15); inj.cmd(key=[0x04, False]); time.sleep(0.6)
    keys = [e for e in events("/tmp/gn-b/observer.log")[before:] if e["type"] == "key"]
    step("a key pressed on A arrives in the window on B as 'a' (HID 0x04 → evdev 30 → X keycode 38, keyval 97)",
         [(k["keycode"], k["keyval"], k["down"]) for k in keys] == [(38, 97, True), (38, 97, False)], json.dumps(keys))
    before = len(events("/tmp/gn-b/observer.log"))
    inj.cmd(button=[1, True]); time.sleep(0.15); inj.cmd(button=[1, False]); time.sleep(0.6)
    buttons = [e for e in events("/tmp/gn-b/observer.log")[before:] if e["type"] == "button"]
    step("a click on A arrives in the window on B as button 1 press + release", [(x["button"], x["down"]) for x in buttons] == [(1, True), (1, False)], json.dumps(buttons))
    step("B's window shows A controlling it", b.wait(lambda s: any(x["peer_id"] == ida and x["remote_control"] for x in s["sessions"]), 5))

    # Back across the edge.
    for _ in range(5):
        inj.cmd(rel=[-60, 0]); time.sleep(0.1)
    step("moving back past B's left edge returns the pointer: A no longer drives B", a.wait(lambda s: s.get("driving") is None, 10))
    step("B's window no longer shows A controlling it", b.wait(lambda s: not any(x["remote_control"] for x in s["sessions"]), 10))
    step("capture session survived the round trip (still 'ready' on Mutter A)", a.state["input"]["capture"]["state"] == "ready", json.dumps(a.state["input"]["capture"]))

    # Second crossing after the first: Mutter re-arms the barrier.
    inj.cmd(abs=[1800, 300]); time.sleep(0.3)
    for _ in range(6):
        inj.cmd(rel=[40, 0]); time.sleep(0.1)
    step("a second crossing works (barrier re-armed after Release)", a.wait(lambda s: s.get("driving") == idb, 10))
    for _ in range(5):
        inj.cmd(rel=[-60, 0]); time.sleep(0.1)
    a.wait(lambda s: s.get("driving") is None, 10)

    a.quit(); b.quit()
    step("agents exit cleanly", a.proc.returncode == 0 and b.proc.returncode == 0)
    A.stop(); B.stop()
    sys.exit(step.finish())
