"""The README's screenshots: the Devices page with a Mac placed to the left
and connected, in each language, light and dark. The window runs against a
stand-in agent (the one of e2e_canvas.py), so nothing of this machine shows.

Run inside an X display (`make screenshots` uses xvfb-run):
screenshots.py <window> <agent> <output directory>"""
import json, os, subprocess, sys, tempfile, time
sys.path.insert(0, os.path.dirname(__file__))
from e2e_canvas import FakeAgent, template, LOCAL, MAC

WINDOW, AGENT, OUT = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2]), os.path.abspath(sys.argv[3])
# The window's default size, drawn at twice the density.
SCALE, WIDTH, HEIGHT = 2, 780, 680


def shot(base, language, theme):
    state = dict(base)
    state["device_name"] = "Workstation"
    state["settings"] = dict(base["settings"], language=language, theme=theme)
    state["layout"] = {"displays": [LOCAL], "devices": [
        {"peer_id": "mac", "name": "MacBook Pro", "platform": "macos", "displays": MAC, "offset": {"x": -1512, "y": 49},
         "side": "left", "remembered": True, "last_seen_ms": None},
    ]}
    state["peers"] = [{"id": "mac", "name": "MacBook Pro", "platform": "macos", "address": "192.0.2.1", "port": 42421}]
    state["sessions"] = [{"peer_id": "mac", "name": "MacBook Pro", "status": "connected", "remote_control": False,
                          "latency_ms": 3, "paired": True}]
    state["paired_peers"] = ["mac"]
    for side in ("capture", "emulation"):
        state["input"][side] = {"state": "ready", "backend": "portal", "detail": ""}
    state["last_error"] = None

    root = tempfile.mkdtemp(prefix="rinflow-shots-")
    home, runtime = os.path.join(root, "home"), os.path.join(root, "run")
    os.makedirs(home)
    os.makedirs(runtime, mode=0o700)
    # The window reads its theme and language from the settings file
    # before it starts.
    data = os.path.join(home, ".local", "share", "rinflow")
    os.makedirs(data)
    with open(os.path.join(data, "settings.json"), "w") as f:
        json.dump(state["settings"], f)
    FakeAgent(os.path.join(runtime, "rinflow", "agent.sock"), state)
    env = dict(os.environ, HOME=home, XDG_RUNTIME_DIR=runtime, XDG_CONFIG_HOME=os.path.join(home, ".config"),
               GDK_BACKEND="x11", GDK_SCALE=str(SCALE), LANG="C.UTF-8", GSETTINGS_BACKEND="memory",
               # libadwaita's own colours, not this machine's accent.
               ADW_DISABLE_PORTAL="1")
    for name in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "WAYLAND_DISPLAY", "XDG_DATA_HOME"):
        env.pop(name, None)
    window = subprocess.Popen([WINDOW], env=env, stderr=subprocess.DEVNULL)
    try:
        # The first window waits for the session's services to start.
        deadline = time.time() + 90
        while time.time() < deadline:
            tree = subprocess.run(["xwininfo", "-root", "-tree"], capture_output=True, text=True).stdout
            if '"Rinflow"' in tree:
                break
            time.sleep(0.5)
        else:
            raise SystemExit("the window did not appear")
        time.sleep(3)
        path = os.path.join(OUT, f"devices-{language}-{theme}.png")
        subprocess.run(["import", "-window", "root", "-crop", f"{WIDTH * SCALE}x{HEIGHT * SCALE}+0+0", "+repage", path], check=True)
        print(path)
    finally:
        window.terminate()
        window.wait(timeout=10)


if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    base = template(AGENT)
    for language in ("en-US", "zh-CN", "ja-JP"):
        for theme in ("light", "dark"):
            shot(base, language, theme)
