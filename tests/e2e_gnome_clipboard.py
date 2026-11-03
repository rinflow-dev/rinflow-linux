"""The clipboard on two real GNOME desktops (Mutter headless + the GNOME portal
backend, as in e2e_gnome.py): a GTK application copies on one, the pointer
crosses, a GTK application pastes on the other, through Mutter's own
clipboard and the Clipboard portal of each agent's RemoteDesktop session.

  clipapp A ─copy→ Mutter A ─portal→ agent A ─QUIC→ agent B ─portal→ Mutter B ─paste→ clipapp B
  (and an image back the other way when the pointer returns)

Usage: e2e_gnome_clipboard.py <agent> <injector>; needs what e2e_gnome.py needs."""
import json, os, struct, sys, time, zlib
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from agentclient import Agent, Steps
from e2e_gnome import Control, Stack, wait

step = Steps()

TEXT = "héllo 你好 — copied in GTK on GNOME A"
HTML = "<p>Some <b>bold</b> HTML</p>"


def png(width, height, rgba):
    """A PNG of one colour, made here so the test knows every byte of it."""
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    row = b"\x00" + bytes(rgba) * width
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(row * height)) + chunk(b"IEND", b""))


class App(Control):
    """clipapp.py on one desktop."""
    def ask(self, **kw):
        return self.cmd(**kw)


if __name__ == "__main__":
    print("bringing up two GNOME desktops…", flush=True)
    A = Stack("clipa", "wayland-7")
    B = Stack("clipb", "wayland-6", size="1600x900")
    a = Agent("gca", A.home, A.runtime, sys.argv[1], port=42491, env=A.env, fresh=False)
    b = Agent("gcb", B.home, B.runtime, sys.argv[1], port=42501, env=B.env, fresh=False)
    ida, idb = a.state["device_id"], b.state["device_id"]
    ready = lambda s: s["input"]["capture"]["state"] == "ready" and s["input"]["emulation"]["state"] == "ready"
    print("  dialogs A:", A.allow_until(a, ready), "| dialogs B:", B.allow_until(b, ready), flush=True)
    step("capture and emulation ready on both real desktops", a.wait(ready, 5) and b.wait(ready, 5))
    step("GNOME gave each RemoteDesktop session its clipboard",
         a.wait(lambda s: s["input"]["clipboard"], 10) and b.wait(lambda s: s["input"]["clipboard"], 10),
         f"A={a.state['input']['clipboard']} B={b.state['input']['clipboard']}")

    # The pointer on A is pushed by an injector, as in e2e_gnome.py; B's is
    # parked away from its own left edge.
    A.spawn([sys.argv[2], f"{A.runtime}/injector.sock"], "injector"); time.sleep(2); A.allow(10)
    inj = Control(f"{A.runtime}/injector.sock")
    wait(lambda: inj.cmd(state=True).get("ready"), 15)
    B.spawn([sys.argv[2], f"{B.runtime}/injector.sock"], "injector"); time.sleep(2); B.allow(10)
    inj_b = Control(f"{B.runtime}/injector.sock")
    wait(lambda: inj_b.cmd(state=True).get("ready"), 15)
    inj_b.cmd(abs=[800, 450]); time.sleep(0.3)

    A.spawn(["python3", f"{HERE}/clipapp.py", f"{A.runtime}/clipapp.sock"], "clipapp")
    B.spawn(["python3", f"{HERE}/clipapp.py", f"{B.runtime}/clipapp.sock"], "clipapp")
    app_a, app_b = App(f"{A.runtime}/clipapp.sock"), App(f"{B.runtime}/clipapp.sock")
    step("a GTK application is up on each desktop", "formats" in app_a.ask(formats=True) and "formats" in app_b.ask(formats=True))

    step("discovery", a.wait(lambda s: any(p["id"] == idb for p in s["peers"])) and b.wait(lambda s: any(p["id"] == ida for p in s["peers"])))
    a.send({"type": "connect", "peer_id": idb})
    if a.wait(lambda s: s.get("pairing"), 15) and b.wait(lambda s: s.get("pairing"), 15):
        a.send({"type": "accept_pairing", "peer_id": idb}); b.send({"type": "accept_pairing", "peer_id": ida})
    step("paired and connected", a.wait(lambda s: any(x["peer_id"] == idb and x["status"] == "connected" for x in s["sessions"])) and
         b.wait(lambda s: any(x["peer_id"] == ida and x["status"] == "connected" for x in s["sessions"])))
    a.send({"type": "place_beside", "peer_id": idb, "side": "right"})
    step("B placed right of A", a.wait(lambda s: any(d["peer_id"] == idb and d.get("offset") for d in s["layout"]["devices"]), 10))
    for agent in (a, b):
        agent.send({"type": "update_settings", "values": {"clipboard": True}})
    step("clipboard sharing on, both sides", a.wait(lambda s: s["settings"]["clipboard"]) and b.wait(lambda s: s["settings"]["clipboard"]))
    time.sleep(2)

    def cross(y):
        inj.cmd(abs=[1800, y]); time.sleep(0.3)
        for _ in range(6):
            inj.cmd(rel=[40, 0]); time.sleep(0.1)
        return a.wait(lambda s: s.get("driving") == idb, 10)

    def click():
        """A click where the pointer is: Wayland lets an application set the
        clipboard only in answer to input, as when a user presses Ctrl+C."""
        inj.cmd(button=[1, True]); time.sleep(0.1); inj.cmd(button=[1, False]); time.sleep(0.4)

    def back():
        for _ in range(6):
            inj.cmd(rel=[-60, 0]); time.sleep(0.1)
        return a.wait(lambda s: s.get("driving") is None, 10)

    # Text, A → B.
    inj.cmd(abs=[900, 500]); time.sleep(0.3); click()
    step("GTK on A copies text", app_a.ask(copy_text=TEXT).get("ok") and wait(lambda: app_a.ask(formats=True)["formats"], 5))
    time.sleep(1)
    step("the pointer crosses from A into B", cross(500))
    step("Mutter B now offers text from Rinflow",
         wait(lambda: "text/plain;charset=utf-8" in app_b.ask(formats=True)["formats"], 10), json.dumps(app_b.ask(formats=True)))
    pasted = app_b.ask(paste_text=True)
    step("GTK on B pastes exactly what was copied on A", pasted.get("text") == TEXT, json.dumps(pasted, ensure_ascii=False))

    # An image, B → A, when the pointer comes back.
    image = png(3, 2, (255, 0, 0, 255))
    click()  # A's mouse, on B: through Rinflow into B's application
    step("GTK on B copies an image", app_b.ask(copy_png=image.hex()).get("ok") and wait(lambda: "image/png" in app_b.ask(formats=True)["formats"], 5))
    time.sleep(1)
    step("the pointer comes back to A", back())
    step("Mutter A now offers an image from Rinflow", wait(lambda: "image/png" in app_a.ask(formats=True)["formats"], 10), json.dumps(app_a.ask(formats=True)))
    got = app_a.ask(paste_png=True)
    step("GTK on A pastes B's image: same size, same colour", got.get("width") == 3 and got.get("height") == 2 and got.get("pixel") == [255, 0, 0, 255], json.dumps(got))

    # HTML with its text fallback, A → B again.
    inj.cmd(abs=[900, 500]); time.sleep(0.3); click()
    step("GTK on A copies HTML with a text fallback", app_a.ask(copy_html=HTML, text="Some bold HTML").get("ok") and wait(lambda: "text/html" in app_a.ask(formats=True)["formats"], 5))
    time.sleep(1)
    step("the pointer crosses again", cross(300))
    step("Mutter B offers the HTML too", wait(lambda: "text/html" in app_b.ask(formats=True)["formats"], 10), json.dumps(app_b.ask(formats=True)))
    step("... and its text", app_b.ask(paste_text=True).get("text") == "Some bold HTML")
    back()

    a.quit(); b.quit()
    step("agents exit cleanly", a.proc.returncode == 0 and b.proc.returncode == 0)
    A.stop(); B.stop()
    sys.exit(step.finish())
