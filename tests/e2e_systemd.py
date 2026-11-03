"""An agent started before any desktop, as the systemd user service is at
login: its own environment names no display, and never will. The desktop
announces itself to the systemd user manager instead (import-environment);
the agent must pick it up from there, and let go when its socket is gone
although the manager still names it (a logout, the Steam Deck switching
modes). tests/fakedesk --systemd stands in for the manager and the portals.

Run: e2e_systemd.py <agent binary> <fakedesk binary>. Needs dbus-daemon."""
import os, socket, sys
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps
from fakedeskclient import Fake, session_bus

AGENT, FAKEDESK = sys.argv[1], sys.argv[2]
step = Steps()
RUNTIME = "/tmp/run-sd"

if __name__ == "__main__":
    bus = session_bus()
    fake = Fake(FAKEDESK, "sd-fake", bus, "/tmp/sd-fake.sock", "1920x1080", extra=["--systemd"])
    # The service's environment: a session bus, no display of any kind.
    a = Agent("sd", "/tmp/home-sd", RUNTIME, AGENT, port=42521,
              env={"DBUS_SESSION_BUS_ADDRESS": bus, "WAYLAND_DISPLAY": None, "DISPLAY": None,
                   "XDG_CURRENT_DESKTOP": None, "XDG_SESSION_TYPE": None})
    nothing = lambda s: s["input"]["capture"]["state"] == "unavailable" and s["input"]["emulation"]["state"] == "unavailable"
    step("started before the desktop: no graphical session yet", a.wait(nothing, 10), str(a.state["input"]))
    step("... and nothing was asked of the portals", fake.state()["capture_session"] is None and fake.state()["emulation_session"] is None)

    # The desktop comes up: its Wayland socket appears, and it tells systemd.
    display = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    display.bind(f"{RUNTIME}/wayland-late")
    display.listen(1)
    fake.cmd(cmd="systemd_env", env=["WAYLAND_DISPLAY=wayland-late", "XDG_CURRENT_DESKTOP=GNOME", "XDG_SESSION_TYPE=wayland"])
    ready = lambda s: s["input"]["capture"]["state"] == "ready" and s["input"]["emulation"]["state"] == "ready"
    step("the agent finds the desktop through the systemd manager and starts both backends", a.wait(ready, 20), str(a.state["input"]))
    step("... with sessions on the desktop's portals", fake.state()["capture_session"] is not None and fake.state()["emulation_session"] is not None)

    # The desktop goes away; the manager still names it.
    display.close()
    os.unlink(f"{RUNTIME}/wayland-late")
    step("its socket gone, the agent lets go although systemd still names it", a.wait(nothing, 20), str(a.state["input"]))

    a.quit()
    step("agent exits cleanly", a.proc.returncode == 0)
    fake.stop()
    log = open("/tmp/sd.log").read()
    step("the log tells what happened", "desktop changed: None → Wayland" in log and "desktop changed: Wayland → None" in log)
    sys.exit(step.finish())
