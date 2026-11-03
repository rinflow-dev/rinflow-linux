"""Presses the affirmative button of every portal permission dialog on the
session bus's accessibility bus (headless GNOME in the test container).
Usage: a11y_allow.py [--dump] [timeout seconds]"""
import re, sys, time
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi

AFFIRMATIVE = re.compile(r"^(allow|share|accept|ok|_?allow)$", re.I)
NEGATIVE = re.compile(r"cancel|deny|don", re.I)
dump = "--dump" in sys.argv
timeout = float([a for a in sys.argv[1:] if not a.startswith("--")][0]) if [a for a in sys.argv[1:] if not a.startswith("--")] else 15.0


def walk(node, depth=0, out=None):
    out = out if out is not None else []
    try:
        count = node.get_child_count()
    except Exception:
        return out
    for i in range(count):
        try:
            child = node.get_child_at_index(i)
            if child is None:
                continue
            out.append((depth, child))
            walk(child, depth + 1, out)
        except Exception:
            pass
    return out


def press(button):
    try:
        action = button.get_action_iface()
        if action is None:
            return False
        for i in range(action.get_n_actions()):
            name = action.get_action_name(i) or ""
            if name in ("click", "activate", "press"):
                action.do_action(i)
                return True
        action.do_action(0)
        return True
    except Exception as error:
        print("press failed:", error)
        return False


pressed = 0
end = time.time() + timeout
while time.time() < end:
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app is None or "portal" not in (app.get_name() or ""):
            continue
        for j in range(app.get_child_count()):
            window = app.get_child_at_index(j)
            if window is None:
                continue
            nodes = walk(window)
            if dump:
                print("WINDOW:", window.get_name(), window.get_role_name())
                for depth, node in nodes:
                    print("  " * depth, node.get_role_name(), repr(node.get_name()), node.get_state_set().contains(Atspi.StateType.SENSITIVE) and "sensitive" or "")
            # GTK 4.14 says "push button" / "check box"; GTK 4.22 says "button" / "switch".
            buttons = [n for _, n in nodes if n.get_role_name() in ("push button", "button", "toggle button")]
            # Device toggles and "Remember this selection" first, so nothing is left off.
            for _, node in nodes:
                if node.get_role_name() in ("check box", "switch") and not node.get_state_set().contains(Atspi.StateType.CHECKED):
                    press(node)
                    time.sleep(0.1)
            for button in buttons:
                name = (button.get_name() or "").strip()
                if AFFIRMATIVE.match(name) and not NEGATIVE.search(name):
                    if press(button):
                        print(f"pressed '{name}' in '{window.get_name()}'", flush=True)
                        pressed += 1
    if pressed:
        time.sleep(0.5)
        # Any dialog that opened meanwhile gets the next round.
        if not any("portal" in (Atspi.get_desktop(0).get_child_at_index(i).get_name() or "") and Atspi.get_desktop(0).get_child_at_index(i).get_child_count() > 0 for i in range(Atspi.get_desktop(0).get_child_count())):
            break
    time.sleep(0.5)
print("pressed", pressed)
sys.exit(0 if pressed else 1)
