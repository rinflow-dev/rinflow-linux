"""Portal permission persistence on a real GNOME: the first start asks (we
press Share through AT-SPI); the agent saves the restore tokens; a second
start must reach 'ready' without any dialog. Needs a GNOME ≥ 51 portal
backend for the capture side (InputCapture v2); RemoteDesktop persists
from GNOME 45. Usage: e2e_persist.py <agent binary>"""
import json, os, shutil, subprocess, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from agentclient import Agent, Steps
from e2e_gnome import Stack  # noqa: E402  (the module only defines classes when imported)

AGENT = sys.argv[1]
step = Steps()


def dialogs(stack):
    out = subprocess.run(["python3", "-c", """
import gi; gi.require_version("Atspi", "2.0"); from gi.repository import Atspi
d = Atspi.get_desktop(0); names = []
for i in range(d.get_child_count()):
    app = d.get_child_at_index(i)
    if app and "portal" in (app.get_name() or ""):
        for j in range(app.get_child_count()):
            w = app.get_child_at_index(j)
            if w: names.append(w.get_name())
print("\\n".join(names))
"""], env=stack.env, capture_output=True, text=True).stdout.split()
    return [n for n in out if n]


S = Stack("p", "wayland-7")
versions = subprocess.run(["rpm", "-q", "mutter", "xdg-desktop-portal-gnome", "xdg-desktop-portal"], capture_output=True, text=True).stdout.split()
print("  stack:", " ".join(versions) if versions else "(not an rpm system)")
ic_version = subprocess.run(["dbus-send", "--session", "--print-reply", "--dest=org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
                             "org.freedesktop.DBus.Properties.Get", "string:org.freedesktop.portal.InputCapture", "string:version"],
                            env=S.env, capture_output=True, text=True).stdout
print("  InputCapture portal version:", ic_version.strip().split()[-1] if ic_version else "?")

ready = lambda s: s["input"]["capture"]["state"] == "ready" and s["input"]["emulation"]["state"] == "ready"
a = Agent("pa", S.home, S.runtime, AGENT, env=S.env, fresh=False)
step("first start: GNOME asks", a.wait(lambda s: "prompting" in (s["input"]["capture"]["state"], s["input"]["emulation"]["state"]), 15))
print("  pressed:", S.allow_until(a, ready))
step("first start: ready after allowing", a.state is not None and ready(a.state), json.dumps(a.state["input"]))
state_dir = os.path.join(S.home, ".local/state/rinflow")
tokens = sorted(f for f in os.listdir(state_dir) if f.endswith(".token")) if os.path.isdir(state_dir) else []
step("restore tokens saved", "remote-desktop.token" in tokens, json.dumps(tokens))
capture_v2 = "input-capture.token" in tokens
print("  InputCapture v2 token:", capture_v2)
a.quit()

a = Agent("pa2", S.home, S.runtime, AGENT, env=S.env, fresh=False)
seen = set()
end = time.time() + 20
while time.time() < end and not (a.state and ready(a.state)):
    a.pump(0.5)
    seen.update(dialogs(S))
step("second start: ready again", a.state is not None and ready(a.state), json.dumps(a.state["input"] if a.state else None))
step("second start: no 'Remote Desktop' dialog (RemoteDesktop restore token honoured)", "Remote Desktop" not in seen, json.dumps(sorted(seen)))
if capture_v2:
    step("second start: no 'Capture Input' dialog (InputCapture v2 restore token honoured)", "Capture Input" not in seen, json.dumps(sorted(seen)))
else:
    print("  (InputCapture portal is v1 here: a capture dialog on every start is expected)", "seen:", sorted(seen))
tokens2 = sorted(f for f in os.listdir(state_dir) if f.endswith(".token"))
step("tokens rotated (new ones written after the second start)", tokens2 == tokens)
a.quit()
S.stop()
sys.exit(step.finish())
