"""The whole crossing, through two real agents and the real core, with a
fake desktop on each side that speaks the real portal and libei protocols:

  fakedesk A  --portal/EIS-->  agent A  --QUIC-->  agent B  --EIS-->  fakedesk B
  (pointer hits the barrier)  (drives B)          (plays it)   (records what arrived)

Run inside the build container: e2e_crossing.py <agent binary> <fakedesk binary>.
Needs dbus-daemon."""
import json, os, sys
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps
from fakedeskclient import Fake, session_bus

AGENT, FAKEDESK = sys.argv[1], sys.argv[2]
step = Steps()


if __name__ == "__main__":
    bus_a, bus_b = session_bus(), session_bus()
    fake_a = Fake(FAKEDESK, "fake-a", bus_a, "/tmp/fake-a.sock", "1920x1080")
    fake_b = Fake(FAKEDESK, "fake-b", bus_b, "/tmp/fake-b.sock", "1600x900")
    a = Agent("xa", "/tmp/home-xa", "/tmp/run-xa", AGENT, port=42411, env={"DBUS_SESSION_BUS_ADDRESS": bus_a, "WAYLAND_DISPLAY": "wayland-fake"})
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
    step("barriers are not set again while nothing changed (agent events re-check them often)",
         fake_a.state()["refused_sets"] == 0, json.dumps({k: fake_a.state()[k] for k in ("barrier_sets", "refused_sets")}))
    # Moving B changes the barrier while the session is enabled: the agent
    # must disable it first, as Mutter refuses barriers on an enabled session.
    a.send({"type": "place_beside", "peer_id": idb, "side": "left"})
    step("B moved to the left: the barrier moves to A's left edge, session enabled again",
         fake_a.wait(lambda s: s["enabled"] and any(bar[1] == 0 and bar[3] == 0 for bar in s["barriers"]), 10), json.dumps(fake_a.state()["barriers"]))
    a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
    step("... and back to the right", fake_a.wait(lambda s: s["enabled"] and any(bar[1] == 1920 for bar in s["barriers"]), 10))
    step("no barriers were refused", fake_a.state()["refused_sets"] == 0, str(fake_a.state()["refused_sets"]))
    offset_b = next(d["offset"] for d in a.state["layout"]["devices"] if d["peer_id"] == idb)
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

    # --- Locking the pointer to its screen ---------------------------------
    import subprocess, time
    CTL = os.path.join(os.path.dirname(AGENT), "rinflowctl")
    def ctl(*words):
        done = subprocess.run([CTL, *words], env=a.env, capture_output=True, text=True, timeout=30)
        return done.returncode, done.stdout + done.stderr
    has_barrier = lambda s: s["enabled"] and any(bar[1] == 1920 for bar in s["barriers"])
    no_barrier = lambda s: s["barriers"] == [] or not s["enabled"]
    def home():
        a.send({"type": "home"})
        return a.wait(lambda s: s.get("driving") is None, 10)
    def cross(y=500):
        barrier = next(bar[0] for bar in fake_a.state()["barriers"] if bar[1] == 1920)
        fake_a.cmd(cmd="activate", barrier=barrier, x=1920, y=y)

    a.send({"type": "set_locked", "locked": True})
    step("locked at home: the barriers are gone", a.wait(lambda s: s.get("locked") is True, 5) and fake_a.wait(no_barrier, 10))
    code, out = ctl("lock", "off")
    step("rinflowctl lock off frees it: barriers back", code == 0 and a.wait(lambda s: s.get("locked") is False, 5) and fake_a.wait(has_barrier, 10), out)

    cross()
    step("crossing for the lock test", a.wait(lambda s: s.get("driving") == idb, 10))
    keys_before = len(received("key"))
    fake_a.cmd(cmd="key", code=70, down=True); fake_a.cmd(cmd="key", code=70, down=False)
    step("Scroll Lock while driving locks the pointer to the peer", a.wait(lambda s: s.get("locked") is True, 5))
    releases = len(fake_a.state()["releases"])
    fake_a.cmd(cmd="motion", dx=-600, dy=0)
    a.pump(1.0)
    step("... and the pointer stays there however far it is pushed home", a.state.get("driving") == idb and len(fake_a.state()["releases"]) == releases)
    step("... and Scroll Lock itself is not sent on", len(received("key")) == keys_before, json.dumps(received("key")[keys_before:]))
    fake_a.cmd(cmd="key", code=70, down=True); fake_a.cmd(cmd="key", code=70, down=False)
    step("Scroll Lock again frees it", a.wait(lambda s: s.get("locked") is False, 5))
    fake_a.cmd(cmd="motion", dx=-600, dy=0)
    step("... and the pointer comes home across the edge", a.wait(lambda s: s.get("driving") is None, 10))
    step("barriers are still there after coming home", fake_a.wait(has_barrier, 10))

    # --- Against crossing by accident --------------------------------------
    a.send({"type": "update_settings", "values": {"corner_guard": 100}})
    a.wait(lambda s: s["settings"].get("corner_guard") == 100, 5)
    releases = len(fake_a.state()["releases"])
    cross(y=20)
    step("an edge hit inside the corner margin is given straight back", fake_a.wait(lambda s: len(s["releases"]) > releases, 10) and a.state.get("driving") is None)
    cross(y=500)
    step("... away from the corner it crosses", a.wait(lambda s: s.get("driving") == idb, 10))
    step("the home request brings the pointer back", home())
    a.send({"type": "update_settings", "values": {"corner_guard": 0, "switch_delay_ms": 400}})
    a.wait(lambda s: s["settings"].get("switch_delay_ms") == 400, 5)

    releases = len(fake_a.state()["releases"])
    cross()
    time.sleep(0.1)
    step("with a crossing delay the hit does not cross at once", len(fake_a.state()["releases"]) == releases and a.state.get("driving") is None)
    step("... and a pointer that stops pushing is given back", fake_a.wait(lambda s: len(s["releases"]) > releases, 5) and a.state.get("driving") is None)
    releases = len(fake_a.state()["releases"])
    cross()
    for _ in range(4):
        fake_a.cmd(cmd="motion", dx=4, dy=0); time.sleep(0.05)
    fake_a.cmd(cmd="motion", dx=-30, dy=0)
    step("... so is one pulled back from the edge", fake_a.wait(lambda s: len(s["releases"]) > releases, 5) and a.state.get("driving") is None)
    cross()
    started = time.time()
    crossed = False
    # (Pushing every 50 ms; the agent's socket is only read afterwards, as
    # reading it waits up to half a second.)
    while time.time() - started < 0.8:
        fake_a.cmd(cmd="motion", dx=4, dy=0)
        time.sleep(0.05)
    crossed = a.wait(lambda s: s.get("driving") == idb, 3)
    took = 0.4
    step("... and one that keeps pushing crosses after the delay", crossed)
    step("home again", home())
    a.send({"type": "update_settings", "values": {"switch_delay_ms": 0}})
    code, out = ctl("set", "switch_delay_ms", "5000")
    step("a delay out of range is refused, with the reason", code != 0 and "at most" in out, out)

    # --- rinflowctl ----------------------------------------------------------
    name_b = b.state["device_name"]
    code, out = ctl("status")
    step("rinflowctl status lists the device", code == 0 and idb in out and "free to cross" in out, out)
    code, out = ctl("layout")
    step("rinflowctl layout gives displays and the placement in points", code == 0 and "1920×1080 at 0,0" in out and "at 1920," in out, out)
    code, out = ctl("place", idb[:8], "left")
    step("rinflowctl place <device> left", code == 0 and "on the left" in out and fake_a.wait(lambda s: s["enabled"] and any(bar[1] == 0 for bar in s["barriers"]), 10), out)
    code, out = ctl("place", idb, "--at", "1920,100")
    step("rinflowctl place <device> --at X,Y", code == 0 and "1920," in out and fake_a.wait(has_barrier, 10), out)
    code, out = ctl("unplace", idb)
    step("rinflowctl unplace", code == 0 and fake_a.wait(no_barrier, 10), out)
    code, out = ctl("place", idb, "right")
    step("... and place right again", code == 0 and fake_a.wait(has_barrier, 10), out)
    code, out = ctl("place", "nobody", "left")
    step("an unknown device is an error", code != 0 and "no device" in out, out)

    a.quit(); b.quit()
    step("agents exit cleanly", a.proc.returncode == 0 and b.proc.returncode == 0)
    fake_a.stop(); fake_b.stop()
    sys.exit(step.finish())
