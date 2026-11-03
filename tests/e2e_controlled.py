"""A real desktop as the controlled device: agent B on real Mutter or KWin
(RINFLOW_TEST_DESKTOP), driven across the edge by agent A on a fake desktop.
What the real compositor delivers is recorded by a GTK window on it. A click
follows each batch of motion: headless KWin without a render device only
flushes motion to clients together with the next button.

Usage: e2e_controlled.py <agent> <fakedesk>"""
import json, os, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from agentclient import Agent, Steps
from e2e_gnome import Stack, DESKTOP, events
from e2e_crossing import Fake, session_bus

AGENT, FAKEDESK = sys.argv[1], sys.argv[2]
step = Steps()

B = Stack("c", "wayland-4", size="1600x900")
b = Agent("cb", B.home, B.runtime, AGENT, port=42431, env=B.env, fresh=False)
step(f"{DESKTOP}: agent asks the desktop", b.wait(lambda s: "prompting" in (s["input"]["capture"]["state"], s["input"]["emulation"]["state"]), 15))
print("  pressed:", B.allow_until(b, lambda s: s["input"]["emulation"]["state"] == "ready"))
step(f"{DESKTOP}: emulation ready on the real desktop", b.state["input"]["emulation"]["state"] == "ready", json.dumps(b.state["input"]))
B.spawn(["python3", f"{HERE}/observer.py", f"/tmp/gn-c/observer.log"], "observer")
step("observer window up", any(e["type"] == "ready" for e in (time.sleep(4), events("/tmp/gn-c/observer.log"))[1]))

bus_a = session_bus()
fake_a = Fake("ctl-fake-a", bus_a, "/tmp/ctl-fake-a.sock", "1920x1080")
a = Agent("ca", "/tmp/home-ca", "/tmp/run-ca", AGENT, env={"DBUS_SESSION_BUS_ADDRESS": bus_a, "WAYLAND_DISPLAY": "wayland-fake"})
ida, idb = a.state["device_id"], b.state["device_id"]
step("controller ready", a.wait(lambda s: s["input"]["capture"]["state"] == "ready", 20))
step("discovery", a.wait(lambda s: any(p["id"] == idb for p in s["peers"])) and b.wait(lambda s: any(p["id"] == ida for p in s["peers"])))
a.send({"type": "connect", "peer_id": idb})
if a.wait(lambda s: s.get("pairing"), 15) and b.wait(lambda s: s.get("pairing"), 15):
    a.send({"type": "accept_pairing", "peer_id": idb}); b.send({"type": "accept_pairing", "peer_id": ida})
step("paired and connected", a.wait(lambda s: any(x["peer_id"] == idb and x["status"] == "connected" for x in s["sessions"])))
a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
step("real desktop placed right of the controller", a.wait(lambda s: any(d["peer_id"] == idb and d.get("offset") and d["offset"]["x"] == 1920.0 for d in s["layout"]["devices"]), 10))
offset_b = next(d["offset"] for d in a.state["layout"]["devices"] if d["peer_id"] == idb)
step("the real desktop reported its 1600x900 screen", any(d["peer_id"] == idb and d["displays"][0]["width"] == 1600.0 for d in a.state["layout"]["devices"]))
step("barrier on the controller's right edge", fake_a.wait(lambda s: s["enabled"] and any(bar[1] == 1920 for bar in s["barriers"])))
barrier = next(bar[0] for bar in fake_a.state()["barriers"] if bar[1] == 1920)

def positions_since(n):
    return [(e["type"], e.get("x"), e.get("y")) for e in events("/tmp/gn-c/observer.log")[n:] if e["type"] in ("motion", "enter")]
n = len(events("/tmp/gn-c/observer.log"))
fake_a.cmd(cmd="activate", barrier=barrier, x=1920, y=500)
step("crossing: controller drives the real desktop", a.wait(lambda st: st.get("driving") == idb, 10))
time.sleep(0.5)
fake_a.cmd(cmd="motion", dx=60, dy=0); time.sleep(0.2); fake_a.cmd(cmd="motion", dx=0, dy=40); time.sleep(0.3)
fake_a.cmd(cmd="button", code=0x110, down=True); fake_a.cmd(cmd="button", code=0x110, down=False); time.sleep(0.8)
seen = positions_since(n)
want = (2 + 60, 500 - offset_b["y"] + 40)
step(f"the real compositor put the pointer at the entry point plus the motion ({want[0]:.0f}, {want[1]:.0f})",
     bool(seen) and abs(seen[-1][1] - want[0]) < 1 and abs(seen[-1][2] - want[1]) < 1, json.dumps(seen[-2:]))
buttons = [(e["button"], e["down"]) for e in events("/tmp/gn-c/observer.log")[n:] if e["type"] == "button"]
step("the click arrived as button 1", buttons[-2:] == [(1, True), (1, False)], json.dumps(buttons))
n = len(events("/tmp/gn-c/observer.log"))
fake_a.cmd(cmd="key", code=30, down=True); fake_a.cmd(cmd="key", code=30, down=False); time.sleep(0.6)
keys = [(e["keycode"], e["keyval"], e["down"]) for e in events("/tmp/gn-c/observer.log")[n:] if e["type"] == "key"]
step("a key typed on the controller arrives on the real desktop as 'a'", keys == [(38, 97, True), (38, 97, False)], json.dumps(keys))
step("the real desktop's window shows the controller controlling it", b.wait(lambda s: any(x["peer_id"] == ida and x["remote_control"] for x in s["sessions"]), 5))
fake_a.cmd(cmd="motion", dx=-400, dy=0)
step("back across the edge", a.wait(lambda st: st.get("driving") is None, 10))
step("control ended on the real desktop", b.wait(lambda s: not any(x["remote_control"] for x in s["sessions"]), 10))

a.quit(); b.quit(); fake_a.stop(); B.stop()
sys.exit(step.finish())
