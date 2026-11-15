"""The window changes language while it runs: the template's text, the lists
and the agent's messages (translated by the window) follow the `language`
setting at once, without a restart. The window is read through AT-SPI.

Run inside an X display with a session bus (the Makefile's e2e-window-i18n
uses xvfb-run and dbus-run-session), after `make locale`:
e2e_window_i18n.py <window binary> [screenshot directory]"""
import json, os, shutil, socket, subprocess, sys, tempfile, time
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Steps
from check_po import entries

WINDOW = os.path.abspath(sys.argv[1])
SHOTS = sys.argv[2] if len(sys.argv) > 2 else None
HERE = os.path.dirname(os.path.abspath(__file__))
step = Steps()

# What to look for: a template string, a string from the code, and the
# agent's message (no Wayland session here, so it says X11 isn't supported).
STRINGS = ["My Devices", "Settings", "X11 sessions aren't supported yet; use a Wayland session."]
CATALOGS = {"zh-CN": "zh_CN", "ja-JP": "ja"}


def expected(language):
    if language not in CATALOGS:
        return list(STRINGS)
    translations = {msgid: msgstr for msgid, msgstr, _, _ in entries(os.path.join(HERE, "..", "po", CATALOGS[language] + ".po"))}
    return [translations[s] for s in STRINGS]


def texts():
    """Every name, description and text the accessibility tree shows."""
    found = set()
    def walk(node, depth=0):
        if node is None or depth > 60:
            return
        try:
            for value in (node.get_name(), node.get_description()):
                if value:
                    found.add(value)
            if node.get_text_iface() is not None:
                found.add(Atspi.Text.get_text(node, 0, -1))
            for i in range(node.get_child_count()):
                walk(node.get_child_at_index(i), depth + 1)
        except Exception:
            pass
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        walk(desktop.get_child_at_index(i))
    return found


def wait_for(language, seconds=20):
    wanted = expected(language)
    unwanted = [] if language not in CATALOGS else [s for s in STRINGS if s not in wanted]
    deadline = time.time() + seconds
    while time.time() < deadline:
        shown = texts()
        if all(w in shown for w in wanted) and not any(u in shown for u in unwanted):
            return True, ""
        time.sleep(0.3)
    return False, f"missing {[w for w in wanted if w not in shown]}, still {[u for u in unwanted if u in shown]}"


def screenshot(name):
    if SHOTS:
        os.makedirs(SHOTS, exist_ok=True)
        subprocess.run(["import", "-window", "root", os.path.join(SHOTS, f"{name}.png")], check=False)


class Socket:
    def __init__(self, path, seconds=20):
        deadline = time.time() + seconds
        while True:
            try:
                self.sock = socket.socket(socket.AF_UNIX)
                self.sock.connect(path)
                break
            except OSError:
                if time.time() > deadline:
                    raise
                time.sleep(0.2)

    def send(self, obj):
        self.sock.sendall((json.dumps(obj) + "\n").encode())


if __name__ == "__main__":
    root = tempfile.mkdtemp(prefix="rinflow-window-i18n-")
    home, runtime = os.path.join(root, "home"), os.path.join(root, "run")
    os.makedirs(home)
    os.makedirs(runtime, mode=0o700)
    env = dict(os.environ, HOME=home, XDG_DATA_HOME=os.path.join(home, ".local/share"),
               XDG_CONFIG_HOME=os.path.join(home, ".config"), XDG_STATE_HOME=os.path.join(home, ".local/state"),
               XDG_RUNTIME_DIR=runtime, GDK_BACKEND="x11", LANG="C.UTF-8", GTK_A11Y="atspi", RUST_LOG="info")
    for name in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "WAYLAND_DISPLAY", "RINFLOW_LOCALEDIR"):
        env.pop(name, None)
    # The window starts the agent next to it.
    window = subprocess.Popen([WINDOW], env=env)
    try:
        ok, why = wait_for("en-US", 40)
        step("the window opens in English (C.UTF-8, no setting)", ok, why)
        screenshot("0-en")
        agent = Socket(os.path.join(runtime, "rinflow", "agent.sock"))
        for language in ["ja-JP", "zh-CN", "en-US"]:
            agent.send({"type": "update_settings", "values": {"language": language}})
            ok, why = wait_for(language)
            step(f"{language}: the template, the lists and the agent's message change at once", ok, why)
            screenshot(f"{['ja-JP', 'zh-CN', 'en-US'].index(language) + 1}-{language}")
        step("the window is still running", window.poll() is None)
        agent.send({"type": "quit"})
        window.wait(15)
        step("the window quits with the agent", window.returncode == 0, str(window.returncode))
    finally:
        if window.poll() is None:
            window.kill()
        shutil.rmtree(root, ignore_errors=True)
    sys.exit(step.finish())
