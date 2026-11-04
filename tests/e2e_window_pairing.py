"""The pairing dialog in the window, against a stand-in agent: it asks once,
and an answer is final. Accepted, it does not come back however many
snapshots still carry the pairing (an alert dialog closes on any answer,
and the window used to open a new one at the next snapshot, blocking the
window until the other device answered); a banner waits for the other
device instead, and its Cancel declines. A new code asks anew.

Run inside an X display with a session bus (the Makefile's
e2e-window-pairing uses xvfb-run and dbus-run-session):
e2e_window_pairing.py <window> <agent>"""
import os, shutil, subprocess, sys, tempfile, time
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Steps
from e2e_canvas import FakeAgent, template

WINDOW = os.path.abspath(sys.argv[1])
AGENT = os.path.abspath(sys.argv[2])
step = Steps()
NAME = "ErenのMac mini"


def texts():
    """The names and text of what is on screen. A hidden banner stays in
    the tree, "showing", but folded to no height."""
    found = set()
    def walk(node, depth=0):
        if node is None or depth > 60:
            return
        try:
            if depth > 1 and (not node.get_state_set().contains(Atspi.StateType.SHOWING)
                              or node.get_extents(Atspi.CoordType.WINDOW).height <= 0):
                return
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


def button(label):
    """A showing push button with this label, anywhere in the window."""
    def walk(node, depth=0):
        if node is None or depth > 60:
            return None
        try:
            if (node.get_role() == Atspi.Role.BUTTON and node.get_name() == label
                    and node.get_state_set().contains(Atspi.StateType.SHOWING)):
                return node
            for i in range(node.get_child_count()):
                found = walk(node.get_child_at_index(i), depth + 1)
                if found:
                    return found
        except Exception:
            return None
        return None
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        found = walk(desktop.get_child_at_index(i))
        if found:
            return found
    return None


def press(label, seconds=10):
    deadline = time.time() + seconds
    while time.time() < deadline:
        node = button(label)
        if node is not None:
            Atspi.Action.do_action(node, 0)
            return True
        time.sleep(0.2)
    return False


def shows(predicate, seconds=10):
    deadline = time.time() + seconds
    while time.time() < deadline:
        if predicate(texts()):
            return True
        time.sleep(0.3)
    return False


def taken(fake, kind, seconds=5):
    """The requests of this kind the window sent, waiting for the first."""
    deadline = time.time() + seconds
    found = []
    while time.time() < deadline and not found:
        found = fake.taken(kind)
        time.sleep(0.1)
    return found


def dialog_shown(shown):
    return f"Pair with {NAME}" in shown


def banner_shown(shown):
    return any(text.startswith(f"Waiting for {NAME} to accept") for text in shown)


if __name__ == "__main__":
    state = template(AGENT)
    state["pairing"] = {"peer_id": "mac", "name": NAME, "platform": "macos", "code": "677276", "accepted": False}

    def answer(fake, request):
        pairing = fake.state["pairing"]
        if request["type"] == "accept_pairing" and pairing:
            pairing["accepted"] = True
        elif request["type"] == "reject_pairing":
            fake.state["pairing"] = None
        else:
            return
        fake.send(dict(fake.state, type="state"))

    root = tempfile.mkdtemp(prefix="rinflow-pairing-")
    home, runtime = os.path.join(root, "home"), os.path.join(root, "run")
    os.makedirs(home)
    os.makedirs(runtime, mode=0o700)
    fake = FakeAgent(os.path.join(runtime, "rinflow", "agent.sock"), state, on_request=answer)
    env = dict(os.environ, HOME=home, XDG_RUNTIME_DIR=runtime, XDG_CONFIG_HOME=os.path.join(home, ".config"),
               GDK_BACKEND="x11", LANG="C.UTF-8", GTK_A11Y="atspi", RUST_LOG="info")
    for name in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "WAYLAND_DISPLAY"):
        env.pop(name, None)
    window = subprocess.Popen([WINDOW], env=env)
    try:
        step("the pairing asks in a dialog", shows(dialog_shown, 30))
        step("Accept can be pressed", press("Accept"))
        time.sleep(0.5)
        # The agent keeps sending snapshots with the pairing (accepted) in it.
        for _ in range(4):
            fake.send(dict(fake.state, type="state"))
            time.sleep(0.3)
        accepted = taken(fake, "accept_pairing")
        step("the agent is told once", len(accepted) == 1, str(accepted))
        step("the dialog doesn't come back", not shows(dialog_shown, 2))
        step("a banner waits for the other device", shows(banner_shown))
        step("its Cancel can be pressed", press("Cancel"))
        rejected = taken(fake, "reject_pairing")
        step("... and declines the pairing", [r["peer_id"] for r in rejected] == ["mac"], str(rejected))
        step("the banner goes with the pairing", shows(lambda shown: not banner_shown(shown)))

        # The other device asks again: a new code, a new question.
        fake.state["pairing"] = {"peer_id": "mac", "name": NAME, "platform": "macos", "code": "123456", "accepted": False}
        fake.send(dict(fake.state, type="state"))
        step("a new pairing asks again", shows(dialog_shown))
        step("Reject can be pressed", press("Reject"))
        rejected = taken(fake, "reject_pairing")
        step("... and declines it", len(rejected) == 1, str(rejected))
        step("nothing comes back after Reject", not shows(lambda shown: dialog_shown(shown) or banner_shown(shown), 2))
        step("the window is still running", window.poll() is None)
    finally:
        fake.send(dict(fake.state, type="state", quitting=True))
        try:
            window.wait(10)
        except subprocess.TimeoutExpired:
            window.kill()
        shutil.rmtree(root, ignore_errors=True)
    sys.exit(step.finish())
