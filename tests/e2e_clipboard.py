"""The clipboard following the pointer, through two real agents and the real
core, with a fake desktop on each side speaking the Clipboard portal on the
agents' RemoteDesktop sessions:

  app on A copies ──▶ agent A ── A drives B ──▶ agent B ──▶ app on B pastes
  app on B copies ──▶ agent B ── A comes back ─▶ agent A ──▶ app on A pastes

Also: the setting is off by default and holds on either side, a password
manager's secret stays home, and a peer's clipboard does not echo back.

Run: e2e_clipboard.py <agent binary> <fakedesk binary>. Needs dbus-daemon."""
import json, os, sys
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps
from fakedeskclient import Fake, session_bus

AGENT, FAKEDESK = sys.argv[1], sys.argv[2]
step = Steps()

TEXT = "héllo 你好, from A"
HTML = "<b>bold</b> from A"
# A real (1×1, red) PNG: the agent checks the signature.
PNG = bytes.fromhex("89504e470d0a1a0a0000000d4948445200000001000000010806000000"
                    "1f15c4890000000d4944415478da63f8cfc0f01f0005000201a6"
                    "f9c4b30000000049454e44ae426082")


def hexs(text):
    return text.encode().hex()


def copy(fake, *formats):
    """An application on that desktop copies: (mime, bytes) pairs."""
    return fake.cmd(cmd="copy", formats=[[m, d.hex()] for m, d in formats])["ok"]


def paste(fake, mime, seconds=10):
    """An application on that desktop pastes `mime`: the bytes the agent
    wrote, or None if it reported failure."""
    serial = str(fake.cmd(cmd="paste", mime=mime)["serial"])
    if not fake.wait(lambda s: s["pastes"].get(serial, {}).get("done") is not None, seconds):
        return "no answer"
    result = fake.state()["pastes"][serial]
    return bytes.fromhex(result["data"]) if result["done"] else None


if __name__ == "__main__":
    bus_a, bus_b = session_bus(), session_bus()
    fake_a = Fake(FAKEDESK, "clip-fake-a", bus_a, "/tmp/clip-fake-a.sock", "1920x1080")
    fake_b = Fake(FAKEDESK, "clip-fake-b", bus_b, "/tmp/clip-fake-b.sock", "1600x900")
    a = Agent("ca", "/tmp/home-ca", "/tmp/run-ca", AGENT, port=42451, env={"DBUS_SESSION_BUS_ADDRESS": bus_a, "WAYLAND_DISPLAY": "wayland-fake"})
    b = Agent("cb", "/tmp/home-cb", "/tmp/run-cb", AGENT, port=42461, env={"DBUS_SESSION_BUS_ADDRESS": bus_b, "WAYLAND_DISPLAY": "wayland-fake"})
    ida, idb = a.state["device_id"], b.state["device_id"]

    ready = lambda s: s["input"]["emulation"]["state"] == "ready" and s["input"]["capture"]["state"] == "ready"
    step("both agents ready", a.wait(ready, 20) and b.wait(ready, 20))
    step("the clipboard was asked for with the RemoteDesktop session", fake_a.state()["clipboard_requested"] and fake_b.state()["clipboard_requested"])
    step("the snapshot says the clipboard is available", a.wait(lambda s: s["input"]["clipboard"], 10) and b.wait(lambda s: s["input"]["clipboard"], 10))
    step("sharing the clipboard is off by default", a.state["settings"]["clipboard"] is False and b.state["settings"]["clipboard"] is False)

    # Pair, connect, place B to the right of A.
    step("discovery", a.wait(lambda s: any(p["id"] == idb for p in s["peers"])) and b.wait(lambda s: any(p["id"] == ida for p in s["peers"])))
    a.send({"type": "connect", "peer_id": idb})
    if a.wait(lambda s: s.get("pairing"), 15) and b.wait(lambda s: s.get("pairing"), 15):
        a.send({"type": "accept_pairing", "peer_id": idb}); b.send({"type": "accept_pairing", "peer_id": ida})
    step("paired and connected", a.wait(lambda s: any(x["peer_id"] == idb and x["status"] == "connected" for x in s["sessions"])) and
         b.wait(lambda s: any(x["peer_id"] == ida and x["status"] == "connected" for x in s["sessions"])))
    a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
    step("B placed right of A", fake_a.wait(lambda s: s["enabled"] and any(bar[1] == 1920 for bar in s["barriers"]), 10))
    barrier = next(bar[0] for bar in fake_a.state()["barriers"] if bar[1] == 1920)

    def cross():
        fake_a.cmd(cmd="activate", barrier=barrier, x=1920, y=500)
        return a.wait(lambda s: s.get("driving") == idb, 10)

    def back():
        fake_a.cmd(cmd="motion", dx=-50, dy=0)
        return a.wait(lambda s: s.get("driving") is None, 10)

    # Off: nothing travels.
    copy(fake_a, ("text/plain;charset=utf-8", TEXT.encode()), ("text/plain", TEXT.encode()))
    step("with the setting off, A crosses into B", cross())
    a.pump(1.5); b.pump(0.1)
    step("... and B is offered nothing", fake_b.state()["selection_sets"] == 0, json.dumps(fake_b.state()["offered_types"]))
    step("... and A's agent never even read what was copied", fake_a.state()["reads"] == [], json.dumps(fake_a.state()["reads"]))
    step("back home", back())

    # On, both sides: the clipboard follows the pointer.
    for agent in (a, b):
        agent.send({"type": "update_settings", "values": {"clipboard": True}})
    step("setting on, saved", a.wait(lambda s: s["settings"]["clipboard"]) and b.wait(lambda s: s["settings"]["clipboard"]))
    step("turning it on reads what was already copied", fake_a.wait(lambda s: "text/plain;charset=utf-8" in s["reads"], 10), json.dumps(fake_a.state()["reads"]))
    reads_before = len(fake_a.state()["reads"])
    copy(fake_a, ("text/html", HTML.encode()), ("text/plain;charset=utf-8", TEXT.encode()), ("UTF8_STRING", TEXT.encode()), ("text/plain", TEXT.encode()))
    step("A's agent reads the shared formats as soon as something is copied",
         fake_a.wait(lambda s: {"text/html", "text/plain;charset=utf-8"} <= set(s["reads"][reads_before:]), 10), json.dumps(fake_a.state()["reads"]))
    step("A crosses into B", cross())
    step("B's desktop is offered A's clipboard: HTML first, then the text types",
         fake_b.wait(lambda s: s["offered_types"][:2] == ["text/html", "text/plain;charset=utf-8"] and "UTF8_STRING" in s["offered_types"], 10),
         json.dumps(fake_b.state()["offered_types"]))
    step("an application on B pastes A's text (UTF8_STRING)", paste(fake_b, "UTF8_STRING") == TEXT.encode())
    step("... and A's HTML", paste(fake_b, "text/html") == HTML.encode())
    step("asking for a type that isn't offered fails cleanly", paste(fake_b, "image/png") is None)

    # While A drives B, an application on B copies an image; it comes back with the pointer.
    sets_a = fake_a.state()["selection_sets"]
    copy(fake_b, ("image/png", PNG), ("text/html", b'<img src="x.png">'))
    step("B's agent reads the image", fake_b.wait(lambda s: "image/png" in s["reads"], 10))
    step("A comes back", back())
    step("A's desktop is offered B's clipboard, HTML then PNG",
         fake_a.wait(lambda s: s["selection_sets"] > sets_a and s["offered_types"] == ["text/html", "image/png"], 10), json.dumps(fake_a.state()["offered_types"]))
    step("an application on A pastes B's PNG, byte for byte", paste(fake_a, "image/png") == PNG)

    # No echo: A now holds B's clipboard; crossing again does not send it back.
    sets_b = fake_b.state()["selection_sets"]
    step("A crosses again", cross())
    a.pump(1.5)
    step("B is not handed back its own clipboard", fake_b.state()["selection_sets"] == sets_b)
    step("back home", back())

    # A password manager's secret stays on A.
    sets_b = fake_b.state()["selection_sets"]
    copy(fake_a, ("text/plain;charset=utf-8", b"hunter2"), ("x-kde-passwordManagerHint", b"secret"))
    fake_a.wait(lambda s: "x-kde-passwordManagerHint" in s["reads"], 5)
    step("A crosses with a password on its clipboard", cross())
    a.pump(1.5)
    step("the password is not offered on B", fake_b.state()["selection_sets"] == sets_b and fake_b.state()["offered_types"] != ["text/plain;charset=utf-8"],
         json.dumps(fake_b.state()["offered_types"]))
    step("back home", back())

    # The setting off on the receiving side holds too.
    b.send({"type": "update_settings", "values": {"clipboard": False}})
    b.wait(lambda s: not s["settings"]["clipboard"])
    sets_b = fake_b.state()["selection_sets"]
    copy(fake_a, ("text/plain;charset=utf-8", "second copy".encode()))
    fake_a.wait(lambda s: s["reads"][-1:] == ["text/plain;charset=utf-8"], 5)
    step("B turned it off; A crosses", cross())
    a.pump(1.5); b.pump(0.5)
    step("B's desktop is offered nothing new", fake_b.state()["selection_sets"] == sets_b)
    step("back home", back())

    a.quit(); b.quit()
    step("agents exit cleanly", a.proc.returncode == 0 and b.proc.returncode == 0)
    fake_a.stop(); fake_b.stop()
    sys.exit(step.finish())
