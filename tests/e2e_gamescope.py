"""Steam Deck Gaming Mode, as far as a container goes: a real gamescope
(headless backend, libeis) with a GTK observer window inside it; the agent
finds gamescope's EIS socket by itself and plays input into it. A second
agent on a fake desktop drives it across the edge.

  fakedesk A ─portal/EIS→ agent A ─QUIC→ agent B ─EIS socket→ gamescope ─X11→ observer window

Usage: e2e_gamescope.py <agent> <fakedesk> <injector>"""
import json, os, shutil, socket, subprocess, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from agentclient import Agent, Steps
from fakedeskclient import Fake, session_bus  # noqa: E402

AGENT, FAKEDESK, INJECTOR = sys.argv[1], sys.argv[2], sys.argv[3]
step = Steps()


def events(path):
    out = []
    try:
        for line in open(path):
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


# Gaming Mode side: gamescope with the observer as its (only) client.
G = "/tmp/gs"
shutil.rmtree(G, ignore_errors=True)
os.makedirs(f"{G}/run", mode=0o700); os.makedirs(f"{G}/home")
genv = dict(os.environ, XDG_RUNTIME_DIR=f"{G}/run", HOME=f"{G}/home", LANG="en_US.UTF-8")
genv.pop("WAYLAND_DISPLAY", None); genv.pop("DISPLAY", None)
gs_log = open(f"{G}/gamescope.log", "w")
gamescope = subprocess.Popen(["gamescope", "--backend", "headless", "-W", "1280", "-H", "800", "--",
                              "env", "GDK_BACKEND=x11", "GSK_RENDERER=cairo", "python3", f"{HERE}/observer.py", f"{G}/observer.log"],
                             env=genv, stdout=gs_log, stderr=subprocess.STDOUT)
step("gamescope created its EIS socket", wait(lambda: any(n.endswith("-ei") for n in os.listdir(f"{G}/run")), 20), json.dumps(os.listdir(f"{G}/run")))
eis = next((n for n in os.listdir(f"{G}/run") if n.endswith("-ei")), None)
step("observer window running inside gamescope", wait(lambda: any(e["type"] == "ready" for e in events(f"{G}/observer.log")), 25))

# Our emulation backend straight into gamescope (no portal, no prompt).
inj_log = open(f"{G}/injector.log", "w")
subprocess.Popen([INJECTOR, f"{G}/run/injector.sock", "--socket", f"{G}/run/{eis}"], env=genv, stdout=inj_log, stderr=subprocess.STDOUT)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
for _ in range(50):
    try:
        s.connect(f"{G}/run/injector.sock"); break
    except OSError:
        time.sleep(0.1)
f = s.makefile("rwb", buffering=0)
def inj(**kw):
    s.sendall(json.dumps(kw).encode() + b"\n"); return json.loads(f.readline())
step("libei sender handshake with gamescope's EIS", wait(lambda: inj(state=True).get("ready"), 15))
inj(rel=[30, 20]); time.sleep(0.3); inj(rel=[30, 20]); time.sleep(0.6)
moved = [e for e in events(f"{G}/observer.log") if e["type"] in ("motion", "enter")]
step("relative motion through gamescope reaches the window inside it", len(moved) >= 1, json.dumps(moved[-2:]))
before = len(events(f"{G}/observer.log"))
inj(key=[0x04, True]); time.sleep(0.15); inj(key=[0x04, False]); time.sleep(0.6)
keys = [(e["keycode"], e["keyval"], e["down"]) for e in events(f"{G}/observer.log")[before:] if e["type"] == "key"]
step("a key through gamescope arrives as 'a' (keycode 38 / keyval 97)", keys == [(38, 97, True), (38, 97, False)], json.dumps(keys))
before = len(events(f"{G}/observer.log"))
inj(button=[1, True]); time.sleep(0.15); inj(button=[1, False]); time.sleep(0.6)
buttons = [(e["button"], e["down"]) for e in events(f"{G}/observer.log")[before:] if e["type"] == "button"]
step("a click through gamescope arrives as button 1", buttons == [(1, True), (1, False)], json.dumps(buttons))

# The agent as the controlled device in Gaming Mode: it must find the socket itself.
b = Agent("gsb", f"{G}/home", f"{G}/run", AGENT, port=42431, env=genv, fresh=False)
step("agent detects gamescope: capture 'unavailable' with the Gaming Mode explanation, emulation via the socket",
     b.wait(lambda st: st["input"]["capture"]["state"] == "unavailable" and st["input"]["emulation"]["state"] == "ready" and st["input"]["emulation"]["backend"] == "gamescope", 20),
     json.dumps(b.state["input"]))

# A controller on a fake desktop, placed so that gamescope sits on its right.
bus_a = session_bus()
fake_a = Fake(FAKEDESK, "gs-fake-a", bus_a, "/tmp/gs-fake-a.sock", "1920x1080")
a = Agent("gsa", "/tmp/home-gsa", "/tmp/run-gsa", AGENT, port=42411, env={"DBUS_SESSION_BUS_ADDRESS": bus_a, "WAYLAND_DISPLAY": "wayland-fake"})
ida, idb = a.state["device_id"], b.state["device_id"]
step("controller ready on its desktop", a.wait(lambda st: st["input"]["capture"]["state"] == "ready", 20))
found = a.wait(lambda st: any(p["id"] == idb for p in st["peers"])) and b.wait(lambda st: any(p["id"] == ida for p in st["peers"]))
step("discovery", found)
a.send({"type": "connect", "peer_id": idb})
if a.wait(lambda st: st.get("pairing"), 15) and b.wait(lambda st: st.get("pairing"), 15):
    a.send({"type": "accept_pairing", "peer_id": idb}); b.send({"type": "accept_pairing", "peer_id": ida})
step("paired and connected", a.wait(lambda st: any(x["peer_id"] == idb and x["status"] == "connected" for x in st["sessions"])))
a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
step("gamescope device placed right of the controller", a.wait(lambda st: any(d["peer_id"] == idb and d.get("offset") and d["offset"]["x"] == 1920.0 for d in st["layout"]["devices"]), 10))
offset_b = next(d["offset"] for d in a.state["layout"]["devices"] if d["peer_id"] == idb)
step("the gamescope device reported its 1280x800 screen", any(d["peer_id"] == idb and d["displays"][0]["width"] == 1280.0 for d in a.state["layout"]["devices"]),
     json.dumps([d["displays"] for d in a.state["layout"]["devices"] if d["peer_id"] == idb]))
step("barrier on the controller's right edge", fake_a.wait(lambda st: st["enabled"] and any(bar[1] == 1920 for bar in st["barriers"])))
barrier = next(bar[0] for bar in fake_a.state()["barriers"] if bar[1] == 1920)

before = len(events(f"{G}/observer.log"))
fake_a.cmd(cmd="activate", barrier=barrier, x=1920, y=500)
step("crossing: controller drives the gamescope device", a.wait(lambda st: st.get("driving") == idb, 10))
time.sleep(0.5)
fake_a.cmd(cmd="motion", dx=50, dy=0); time.sleep(0.2); fake_a.cmd(cmd="motion", dx=0, dy=30); time.sleep(0.6)
moved = [e for e in events(f"{G}/observer.log")[before:] if e["type"] in ("motion", "enter")]
step("motion crossed into gamescope and reached the window at the entry height (%.0f)" % (500 - offset_b["y"]),
     len(moved) >= 2 and abs(moved[-1]["y"] - (500 - offset_b["y"] + 30)) < 1 and abs(moved[-1]["x"] - moved[0]["x"] - 50) < 1 + 0 if len(moved) >= 2 else False, json.dumps(moved[:1] + moved[-1:]))
before = len(events(f"{G}/observer.log"))
fake_a.cmd(cmd="key", code=30, down=True); fake_a.cmd(cmd="key", code=30, down=False); time.sleep(0.6)
keys = [(e["keycode"], e["keyval"], e["down"]) for e in events(f"{G}/observer.log")[before:] if e["type"] == "key"]
step("a key typed on the controller arrives inside gamescope", keys == [(38, 97, True), (38, 97, False)], json.dumps(keys))
fake_a.cmd(cmd="motion", dx=-400, dy=0)
step("back across the edge", a.wait(lambda st: st.get("driving") is None, 10))

a.quit(); b.quit(); fake_a.stop()
gamescope.terminate()
sys.exit(step.finish())
