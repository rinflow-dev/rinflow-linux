"""Notifications from the agent while no window is open, against a
stand-in org.freedesktop.Notifications: a pairing request is announced
under Rinflow's name, can be opened or declined but not accepted from
there, and is withdrawn when a window opens or the pairing ends.

Run under a private bus: dbus-run-session -- python3 e2e_notify.py <agent binary>."""
import json, os, socket, stat, sys, threading, time
import gi
from gi.repository import Gio, GLib
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps

AGENT = sys.argv[1]
step = Steps()

XML = """
<node><interface name="org.freedesktop.Notifications">
  <method name="Notify">
    <arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/>
    <arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/>
    <arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/>
  </method>
  <method name="CloseNotification"><arg type="u" direction="in"/></method>
  <method name="GetCapabilities"><arg type="as" direction="out"/></method>
  <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
  <signal name="ActivationToken"><arg type="u"/><arg type="s"/></signal>
  <signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
</interface></node>"""
PATH, IFACE = "/org/freedesktop/Notifications", "org.freedesktop.Notifications"


class Notifications:
    def __init__(self):
        self.shown, self.closed, self.next = [], [], 1
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
        self.bus.register_object(PATH, info, self.call, None, None)
        Gio.bus_own_name_on_connection(self.bus, IFACE, Gio.BusNameOwnerFlags.NONE, None, None)
        threading.Thread(target=GLib.MainLoop().run, daemon=True).start()

    def call(self, bus, sender, path, interface, method, parameters, invocation):
        if method == "Notify":
            app, _, icon, summary, body, actions, hints, timeout = parameters.unpack()
            number, self.next = self.next, self.next + 1
            self.shown.append({"id": number, "app": app, "icon": icon, "summary": summary, "body": body,
                               "actions": actions, "hints": hints, "timeout": timeout})
            invocation.return_value(GLib.Variant("(u)", (number,)))
        elif method == "CloseNotification":
            self.closed.append(parameters.unpack()[0])
            invocation.return_value(None)
        else:
            invocation.return_value(GLib.Variant("(as)", (["actions", "body"],)))

    def press(self, number, action, token=None):
        """The user presses an action (the default one: clicks the notification)."""
        if token:
            self.bus.emit_signal(None, PATH, IFACE, "ActivationToken", GLib.Variant("(us)", (number, token)))
        self.bus.emit_signal(None, PATH, IFACE, "ActionInvoked", GLib.Variant("(us)", (number, action)))


def until(predicate, seconds=10):
    deadline = time.time() + seconds
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def window(agent):
    """A connection that says it is the window."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(agent.sock_path)
    s.sendall(json.dumps({"type": "hello", "protocol": 3, "client": "window"}).encode() + b"\n")
    return s


if __name__ == "__main__":
    service = Notifications()
    # What the agent starts as "the window": notes the activation token.
    opened = "/tmp/notify-window-opened"
    stub = "/tmp/notify-window-stub.sh"
    if os.path.exists(opened):
        os.remove(opened)
    with open(stub, "w") as f:
        f.write(f'#!/bin/sh\necho "token=$XDG_ACTIVATION_TOKEN" > {opened}\n')
    os.chmod(stub, os.stat(stub).st_mode | stat.S_IXUSR)

    quiet = {"WAYLAND_DISPLAY": None, "DISPLAY": None, "LANGUAGE": None, "LC_ALL": None, "LANG": "en_US.UTF-8"}
    # A has no session bus: only B, the device asked, is watched.
    a = Agent("na", "/tmp/home-na", "/tmp/run-na", AGENT, port=42471, env=dict(quiet, DBUS_SESSION_BUS_ADDRESS="unix:path=/nonexistent"))
    b = Agent("nb", "/tmp/home-nb", "/tmp/run-nb", AGENT, port=42481, env=dict(quiet, RINFLOW_WINDOW=stub))
    ida, idb = a.state["device_id"], b.state["device_id"]
    step("discovery", a.wait(lambda s: any(p["id"] == idb for p in s["peers"])))
    asks = lambda s: s.get("pairing") and not s["pairing"]["accepted"]

    # With a window open the window asks; nothing is announced.
    open_window = window(b)
    time.sleep(0.5)
    a.send({"type": "connect", "peer_id": idb})
    step("B is asked to pair", b.wait(asks, 20))
    time.sleep(1.5)
    step("with a window open there is no notification", service.shown == [], str(service.shown))

    # The window closes with the request still waiting: now it is announced.
    open_window.close()
    step("the window gone, the request is announced", until(lambda: len(service.shown) == 1))
    shown = service.shown[0] if service.shown else {"actions": [], "hints": {}, "summary": "", "body": "", "id": 0}
    code = b.state["pairing"]["code"]
    step("... naming the device", shown["summary"] == f"{a.state['device_name']} wants to pair", shown["summary"])
    step("... under Rinflow's desktop entry", shown["hints"].get("desktop-entry") == "com.rinflow.rinflow" and shown["app"] == "Rinflow", str(shown["hints"]))
    step("... not urgent (it must not get past Do Not Disturb)", shown["hints"].get("urgency", 1) != 2)
    step("... it can be opened or declined, not accepted", shown["actions"][0::2] == ["default", "reject"], str(shown["actions"]))
    step("... and does not show the code", code not in shown["summary"] + shown["body"], shown["body"])

    service.press(shown["id"], "default", token="token-123")
    step("a click opens the window, with the activation token", until(lambda: os.path.exists(opened)) and open(opened).read().strip() == "token=token-123",
         open(opened).read() if os.path.exists(opened) else "not opened")
    step("... and the request still waits (opening is not accepting)", b.state.get("pairing") and not b.state["pairing"]["accepted"])

    open_window = window(b)
    step("a window opening withdraws the notification", until(lambda: shown["id"] in service.closed), str(service.closed))
    open_window.close()
    time.sleep(0.5)
    step("... and closing it again does not bring it back", len(service.shown) == 1)

    service.press(shown["id"], "reject")
    step("Reject on the notification declines the pairing", b.wait(lambda s: not s.get("pairing"), 10))
    step("... on the asking device too", a.wait(lambda s: not s.get("pairing"), 10))

    # Asking again at once: one pending request, announced once in a while.
    a.send({"type": "connect", "peer_id": idb})
    step("the same device asks again at once", b.wait(asks, 20))
    time.sleep(1.5)
    step("... and is not announced a second time so soon", len(service.shown) == 1, str([n["summary"] for n in service.shown]))

    a.quit(); b.quit()
    step("agents exit cleanly", a.proc.returncode == 0 and b.proc.returncode == 0)
    sys.exit(step.finish())
