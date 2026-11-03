"""Two agents on one host, through the real core: discovery, pairing,
connection, placement sync, disconnect. Run inside the build container."""
import json, os, sys, time

AGENT = sys.argv[1]
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent as Client, Steps

step = Steps()

a = Client("agent-a", "/tmp/home-a", "/tmp/run-a", AGENT)
b = Client("agent-b", "/tmp/home-b", "/tmp/run-b", AGENT, port=42431)
ida, idb = a.state["device_id"], b.state["device_id"]
step("both agents run the core", a.state["core"] == "running" and b.state["core"] == "running")
step("control ports differ", a.state["control_port"] == 42421 and b.state["control_port"] == 42431,
     f"{a.state['control_port']} / {b.state['control_port']}")

found = a.wait(lambda s: any(p["id"] == idb for p in s["peers"])) and b.wait(lambda s: any(p["id"] == ida for p in s["peers"]))
step("discovery: each agent lists the other as a nearby peer", found)
if found:
    peer = next(p for p in a.state["peers"] if p["id"] == idb)
    step("peer carries platform and address", peer["platform"] == "linux" and peer["port"] == 42431, json.dumps(peer))

a.send({"type": "connect", "peer_id": idb})
pa = a.wait(lambda s: s.get("pairing") and s["pairing"]["peer_id"] == idb)
pb = b.wait(lambda s: s.get("pairing") and s["pairing"]["peer_id"] == ida)
step("pairing requested on both sides", pa and pb)
if pa and pb:
    code_a, code_b = a.state["pairing"]["code"], b.state["pairing"]["code"]
    step("both show the same six-digit code", code_a == code_b and len(code_a) == 6, f"{code_a} / {code_b}")
    a.send({"type": "accept_pairing", "peer_id": idb})
    step("accepting side shows 'accepted' while waiting", a.wait(lambda s: s.get("pairing") and s["pairing"]["accepted"], 5))
    b.send({"type": "accept_pairing", "peer_id": ida})
    paired = a.wait(lambda s: idb in s["paired_peers"] and not s.get("pairing")) and b.wait(lambda s: ida in s["paired_peers"] and not s.get("pairing"))
    step("paired on both sides, dialog gone", paired)

connected = a.wait(lambda s: any(x["peer_id"] == idb and x["status"] == "connected" for x in s["sessions"])) and \
            b.wait(lambda s: any(x["peer_id"] == ida and x["status"] == "connected" for x in s["sessions"]))
step("session connected on both sides", connected)
if connected:
    sa = next(x for x in a.state["sessions"] if x["peer_id"] == idb)
    step("session is marked paired", sa["paired"])
    step("latency reported within a few seconds", a.wait(lambda s: next(x for x in s["sessions"] if x["peer_id"] == idb).get("latency_ms") is not None, 8))
    step("peer remembered as one of my devices", a.wait(lambda s: any(d["peer_id"] == idb and d["remembered"] for d in s["layout"]["devices"]), 5))

a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
placed_a = a.wait(lambda s: any(d["peer_id"] == idb and d.get("offset") for d in s["layout"]["devices"]), 10)
step("placement applied locally", placed_a)
if placed_a:
    off_a = next(d for d in a.state["layout"]["devices"] if d["peer_id"] == idb)["offset"]
    side_a = next(d for d in a.state["layout"]["devices"] if d["peer_id"] == idb)["side"]
    step("placed on the right edge of a 1920-wide screen", off_a == {"x": 1920.0, "y": 0.0} and side_a == "right", f"{off_a} {side_a}")
    mirrored = b.wait(lambda s: any(d["peer_id"] == ida and d.get("offset") == {"x": -off_a["x"], "y": -off_a["y"]} for d in s["layout"]["devices"]), 10)
    step("placement synced to the peer, mirrored", mirrored, json.dumps([d.get("offset") for d in b.state["layout"]["devices"]]))
    with open("/tmp/home-a/.local/share/rinflow/layout.json") as f:
        saved = json.load(f)
    step("placement saved to layout.json", any(r["peer_id"] == idb and r["offset"] for r in saved["placements"]))

a.send({"type": "preview_placement", "peer_id": idb, "x": -1900.0, "y": 100.0})
got = None
end = time.time() + 3
while time.time() < end and got is None:
    try:
        m = a.read()
    except socket.timeout:
        continue
    if m and m["type"] == "placement_preview":
        got = m
    elif m and m["type"] == "state":
        a.state = m
step("placement preview answers with a snapped offset", got is not None and got.get("offset") is not None, json.dumps(got))

a.send({"type": "update_settings", "values": {"sharing": False}})
step("settings change pushed back", a.wait(lambda s: s["settings"]["sharing"] is False, 5))
a.send({"type": "update_settings", "values": {"sharing": True}})
a.wait(lambda s: s["settings"]["sharing"] is True, 5)

a.send({"type": "disconnect", "peer_id": idb})
gone = a.wait(lambda s: not any(x["peer_id"] == idb for x in s["sessions"]), 10) and b.wait(lambda s: not any(x["peer_id"] == ida for x in s["sessions"]), 10)
step("disconnect ends the session on both sides", gone)
step("no error shown for a deliberate disconnect", not a.state.get("last_error") and not b.state.get("last_error"),
     f"{a.state.get('last_error')} / {b.state.get('last_error')}")

a.send({"type": "forget_device", "peer_id": idb})
step("forget removes the device and the pairing", a.wait(lambda s: idb not in s["paired_peers"] and not any(d["peer_id"] == idb and d["remembered"] for d in s["layout"]["devices"]), 10))

a.quit(); b.quit()
step("both agents exit cleanly on quit", a.proc.returncode == 0 and b.proc.returncode == 0, f"{a.proc.returncode} / {b.proc.returncode}")
sys.exit(step.finish())
