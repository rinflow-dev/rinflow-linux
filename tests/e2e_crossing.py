"""The whole crossing, through two real agents and the real core, with a
fake desktop on each side that speaks the real portal and libei protocols:

  fakedesk A  --portal/EIS-->  agent A  --QUIC-->  agent B  --EIS-->  fakedesk B
  (pointer hits the barrier)  (drives B)          (plays it)   (records what arrived)

Run inside the build container: e2e_crossing.py <agent binary> <fakedesk binary>.
Needs dbus-daemon."""
import json, os, socket, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps

AGENT, FAKEDESK = sys.argv[1], sys.argv[2]
step = Steps()


def session_bus():
    out = subprocess.check_output(["dbus-daemon", "--session", "--print-address", "--fork"]).decode().strip()
    return out


class Fake:
    def __init__(self, name, bus, control, zones):
        self.log = open(f"/tmp/{name}.log", "w")
        env = dict(os.environ, DBUS_SESSION_BUS_ADDRESS=bus, RUST_LOG="info")
        self.proc = subprocess.Popen([FAKEDESK, "--control", control, "--zones", zones], env=env,
                                     stdout=self.log, stderr=subprocess.STDOUT)
        for _ in range(50):
            try:
                self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.s.connect(control)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise RuntimeError(f"{name}: control socket never appeared")
        self.f = self.s.makefile("rwb", buffering=0)

    def cmd(self, **kw):
        self.s.sendall(json.dumps(kw).encode() + b"\n")
        return json.loads(self.f.readline())

    def state(self):
        return self.cmd(cmd="state")["state"]

    def wait(self, predicate, seconds=20):
        end = time.time() + seconds
        while time.time() < end:
            if predicate(self.state()):
                return True
            time.sleep(0.2)
        return False

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.log.close()


if __name__ == "__main__":
    bus_a, bus_b = session_bus(), session_bus()
    fake_a = Fake("fake-a", bus_a, "/tmp/fake-a.sock", "1920x1080")
    fake_b = Fake("fake-b", bus_b, "/tmp/fake-b.sock", "1600x900")
    a = Agent("xa", "/tmp/home-xa", "/tmp/run-xa", AGENT, env={"DBUS_SESSION_BUS_ADDRESS": bus_a, "WAYLAND_DISPLAY": "wayland-fake"})
    b = Agent("xb", "/tmp/home-xb", "/tmp/run-xb", AGENT, port=42431, env={"DBUS_SESSION_BUS_ADDRESS": bus_b, "WAYLAND_DISPLAY": "wayland-fake"})
    ida, idb = a.state["device_id"], b.state["device_id"]

    ready = lambda s: s["input"]["capture"]["state"] == "ready" and s["input"]["emulation"]["state"] == "ready"
    step("both agents reach 'ready' on capture and emulation through the portals", a.wait(ready, 20) and b.wait(ready, 20),
         f"A={a.state['input']} B={b.state['input']}" if not (a.state and ready(a.state)) else "")
    fa, fb = fake_a.state(), fake_b.state()
    step("agents connected to the EIS servers (receiver for capture, sender for emulation)", fa["capture_eis"] and fa["emulation_eis"] and fb["capture_eis"] and fb["emulation_eis"])
    step("zones became the agent's displays", a.state["layout"]["displays"] == [{"x": 0.0, "y": 0.0, "width": 1920.0, "height": 1080.0, "scale": 1.0}]
         and b.state["layout"]["displays"][0]["width"] == 1600.0, json.dumps(a.state["layout"]["displays"]))
    token = open("/tmp/home-xb/.local/state/rinflow/remote-desktop.token").read().strip() if os.path.exists("/tmp/home-xb/.local/state/rinflow/remote-desktop.token") else None
    step("RemoteDesktop restore token saved for the next start", token == "fakedesk-restore-token", repr(token))
    step("no barriers yet: nothing is placed", fa["barriers"] == [])

    # Pair, connect, place B to the right of A.
    found = a.wait(lambda s: any(p["id"] == idb for p in s["peers"])) and b.wait(lambda s: any(p["id"] == ida for p in s["peers"]))
    step("discovery", found)
    a.send({"type": "connect", "peer_id": idb})
    if a.wait(lambda s: s.get("pairing"), 15) and b.wait(lambda s: s.get("pairing"), 15):
        a.send({"type": "accept_pairing", "peer_id": idb}); b.send({"type": "accept_pairing", "peer_id": ida})
    connected = a.wait(lambda s: any(x["peer_id"] == idb and x["status"] == "connected" for x in s["sessions"])) and \
                b.wait(lambda s: any(x["peer_id"] == ida and x["status"] == "connected" for x in s["sessions"]))
    step("paired and connected", connected)
    a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
    # B (1600x900) sits against A's right edge; the core centres it vertically.
    step("B placed right of A, snapped by the core", a.wait(lambda s: any(d["peer_id"] == idb and d.get("offset") and d["offset"]["x"] == 1920.0 and d["side"] == "right" for d in s["layout"]["devices"]), 10),
         json.dumps([d.get("offset") for d in a.state["layout"]["devices"]]))
    offset_b = next(d["offset"] for d in a.state["layout"]["devices"] if d["peer_id"] == idb)
    step("A's fake got a barrier on A's right edge, session enabled",
         fake_a.wait(lambda s: s["enabled"] and any(bar[1] == 1920 and bar[3] == 1920 for bar in s["barriers"])), json.dumps(fake_a.state()["barriers"]))
    step("B's fake got a barrier on B's left edge (the mirrored placement)",
         fake_b.wait(lambda s: s["enabled"] and any(bar[1] == 0 and bar[3] == 0 for bar in s["barriers"])), json.dumps(fake_b.state()["barriers"]))
    barrier_id = next(bar[0] for bar in fake_a.state()["barriers"] if bar[1] == 1920)

    # The crossing.
    r = fake_a.cmd(cmd="activate", barrier=barrier_id, x=1920, y=500)
    step("compositor reports the barrier hit", r["ok"])
    step("A drives B", a.wait(lambda s: s.get("driving") == idb, 10), repr(a.state.get("driving")))
    def received(kind):
        return [e for e in fake_b.state()["received"] if e["type"] == kind]
    step("B's compositor saw start_emulating", fake_b.wait(lambda s: any(e["type"] == "start_emulating" for e in s["received"]), 10))
    starts = [e for e in fake_b.state()["received"] if e["type"] == "start_emulating"]
    step("start_emulating exactly once per device (twice is a protocol violation on Mutter)",
         len(starts) == len({e["device"] for e in starts}), json.dumps(starts))
    entry_y = 500 - offset_b["y"]
    step("the pointer entered B at an absolute point just inside its left edge, at the same height in B's space",
         fake_b.wait(lambda s: any(e["type"] == "abs" and 0 <= e["x"] <= 4 and abs(e["y"] - entry_y) < 1 for e in s["received"]), 10), f"expected y={entry_y}: " + json.dumps(received("abs")))
    fake_a.cmd(cmd="motion", dx=100, dy=0); fake_a.cmd(cmd="motion", dx=0, dy=50)
    # The core coalesces consecutive motion before it goes on the wire, so the
    # two moves may arrive as one: the sum is what matters. Only motion after
    # the entry counts (the entry is a corner clamp, a move, then absolute).
    def rel_sum(s):
        received = s["received"]
        first = max((i for i, e in enumerate(received) if e["type"] == "abs"), default=-1) + 1
        moves = [e for e in received[first:] if e["type"] == "rel"]
        return (sum(e["dx"] for e in moves), sum(e["dy"] for e in moves))
    step("relative motion arrives on B, same total (coalesced or not)", fake_b.wait(lambda s: rel_sum(s) == (100.0, 50.0), 10), json.dumps(received("rel")))
    fake_a.cmd(cmd="key", code=30, down=True); fake_a.cmd(cmd="key", code=30, down=False)
    step("a key (evdev 30 = A) arrives on B as evdev 30 press + release, via USB HID on the wire",
         fake_b.wait(lambda s: [(e["code"], e["down"]) for e in s["received"] if e["type"] == "key"][-2:] == [(30, True), (30, False)], 10), json.dumps(received("key")))
    fake_a.cmd(cmd="button", code=0x110, down=True); fake_a.cmd(cmd="button", code=0x110, down=False)
    step("a left button click arrives on B as BTN_LEFT press + release",
         fake_b.wait(lambda s: [(e["code"], e["down"]) for e in s["received"] if e["type"] == "button"][-2:] == [(272, True), (272, False)], 10), json.dumps(received("button")))
    fake_a.cmd(cmd="scroll", dx=0, dy=15)
    step("smooth scrolling arrives on B with libei signs restored", fake_b.wait(lambda s: any(e["type"] == "scroll" and abs(e["dy"] - 15) < 0.01 for e in s["received"]), 10), json.dumps(received("scroll")))
    step("B's window shows A controlling it", b.wait(lambda s: any(x["peer_id"] == ida and x["remote_control"] for x in s["sessions"]), 10))

    # Back across the edge: 2 + 100 - 200 < 0 leaves B through the portal.
    fake_a.cmd(cmd="motion", dx=-200, dy=0)
    step("A gives the pointer back to its compositor just inside its right edge, same height",
         fake_a.wait(lambda s: any(cur and 1900 <= cur[0] <= 1920 and abs(cur[1] - 550) < 1 for _, cur in s["releases"]), 10), json.dumps(fake_a.state()["releases"]))
    step("A no longer drives B", a.wait(lambda s: s.get("driving") is None, 10))
    step("B's window no longer shows A controlling it", b.wait(lambda s: not any(x["remote_control"] for x in s["sessions"]), 10))

    # Sharing off removes the barriers; on brings them back.
    a.send({"type": "update_settings", "values": {"sharing": False}})
    step("sharing off: A's barriers are gone and the session disabled", fake_a.wait(lambda s: s["barriers"] == [] or not s["enabled"], 10))
    a.send({"type": "update_settings", "values": {"sharing": True}})
    step("sharing on: barriers back and enabled", fake_a.wait(lambda s: s["enabled"] and any(bar[1] == 1920 for bar in s["barriers"]), 10))

    # A second crossing, then the escape hatch: Ctrl+Alt+Esc brings the pointer home.
    before = len(fake_a.state()["releases"])
    fake_a.cmd(cmd="activate", barrier=barrier_id, x=1920, y=300)
    step("second crossing starts", a.wait(lambda s: s.get("driving") == idb, 10))
    fake_a.cmd(cmd="key", code=29, down=True); fake_a.cmd(cmd="key", code=56, down=True); fake_a.cmd(cmd="key", code=1, down=True)
    step("Ctrl+Alt+Esc returns the pointer home without crossing", fake_a.wait(lambda s: len(s["releases"]) > before, 10) and a.wait(lambda s: s.get("driving") is None, 5))
    fake_a.cmd(cmd="key", code=1, down=False); fake_a.cmd(cmd="key", code=56, down=False); fake_a.cmd(cmd="key", code=29, down=False)

    a.quit(); b.quit()
    step("agents exit cleanly", a.proc.returncode == 0 and b.proc.returncode == 0)
    fake_a.stop(); fake_b.stop()
    sys.exit(step.finish())
