"""The tray icon, against a stand-in host: the agent registers a
StatusNotifierItem with the watcher, its menu says what is going on, and
its two switches do what the window's do.

Run under a private bus: dbus-run-session -- python3 e2e_tray.py <agent binary>."""
import os, sys, threading, time
import gi
from gi.repository import Gio, GLib
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps

AGENT = sys.argv[1]
step = Steps()

WATCHER_XML = """
<node><interface name="org.kde.StatusNotifierWatcher">
  <method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
  <method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>
  <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
  <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
  <property name="ProtocolVersion" type="i" access="read"/>
  <signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
  <signal name="StatusNotifierHostRegistered"/>
</interface></node>"""


class Watcher:
    """org.kde.StatusNotifierWatcher with a host already there."""
    def __init__(self):
        self.items = []
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        info = Gio.DBusNodeInfo.new_for_xml(WATCHER_XML).interfaces[0]
        self.bus.register_object("/StatusNotifierWatcher", info, self.call, self.get, None)
        Gio.bus_own_name_on_connection(self.bus, "org.kde.StatusNotifierWatcher", Gio.BusNameOwnerFlags.NONE, None, None)
        threading.Thread(target=GLib.MainLoop().run, daemon=True).start()

    def call(self, bus, sender, path, interface, method, parameters, invocation):
        if method == "RegisterStatusNotifierItem":
            service = parameters.unpack()[0]
            # A bus name, or a path on the sender's connection.
            self.items.append((sender, "/StatusNotifierItem") if service.startswith("/") else (service, "/StatusNotifierItem"))
        invocation.return_value(None)

    def get(self, bus, sender, path, interface, name):
        return {"RegisteredStatusNotifierItems": GLib.Variant("as", [i[0] for i in self.items]),
                "IsStatusNotifierHostRegistered": GLib.Variant("b", True),
                "ProtocolVersion": GLib.Variant("i", 0)}[name]


def call(bus, name, path, interface, method, args=None, reply=None):
    return bus.call_sync(name, path, interface, method, args, GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, 5000, None).unpack()


def menu(bus, name, path):
    """[(id, label, enabled, checked)] of the tray's menu."""
    _, (_, _, children) = call(bus, name, path, "com.canonical.dbusmenu", "GetLayout",
                               GLib.Variant("(iias)", (0, -1, [])), "(u(ia{sv}av))")
    rows = []
    for item_id, properties, _ in children:
        if properties.get("type") == "separator":
            continue
        rows.append((item_id, properties.get("label", ""), properties.get("enabled", True), properties.get("toggle-state", -1) == 1))
    return rows


def click(bus, name, path, item_id):
    call(bus, name, path, "com.canonical.dbusmenu", "Event",
         GLib.Variant("(isvu)", (item_id, "clicked", GLib.Variant("i", 0), 0)))


if __name__ == "__main__":
    watcher = Watcher()
    a = Agent("ta", "/tmp/home-ta", "/tmp/run-ta", AGENT, port=42451,
              env={"WAYLAND_DISPLAY": None, "DISPLAY": None, "LANGUAGE": None, "LC_ALL": None, "LANG": "en_US.UTF-8"})
    deadline = time.time() + 10
    while not watcher.items and time.time() < deadline:
        time.sleep(0.1)
    step("the agent registers a tray item with the watcher", bool(watcher.items), str(watcher.items))
    if not watcher.items:
        a.quit(); sys.exit(step.finish())
    name, path = watcher.items[0]
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    properties = call(bus, name, path, "org.freedesktop.DBus.Properties", "GetAll", GLib.Variant("(s)", ("org.kde.StatusNotifierItem",)), "(a{sv})")[0]
    step("it is Rinflow's, with the symbolic icon", properties.get("Id") == "com.rinflow.rinflow" and properties.get("IconName") == "com.rinflow.rinflow-symbolic",
         f"{properties.get('Id')} / {properties.get('IconName')}")
    menu_path = properties.get("Menu")
    rows = menu(bus, name, menu_path)
    labels = [label for _, label, _, _ in rows]
    step("the menu: what is going on, the two switches, the window, quit",
         labels == ["No devices connected", "Share mouse and keyboard", "Keep the pointer on this screen", "Open Rinflow", "Quit"], str(labels))
    step("the first line is not a button; sharing is on, the lock off", rows[0][2] is False and rows[1][3] is True and rows[2][3] is False, str(rows))

    click(bus, name, menu_path, rows[2][0])
    step("the lock switch locks the pointer", a.wait(lambda s: s.get("locked") is True, 5))
    step("... and shows it", any(label.startswith("Keep") and checked for _, label, _, checked in menu(bus, name, menu_path)))
    a.send({"type": "set_locked", "locked": False})
    a.wait(lambda s: s.get("locked") is False, 5)
    time.sleep(0.3)
    step("unlocked elsewhere, the menu follows", not any(label.startswith("Keep") and checked for _, label, _, checked in menu(bus, name, menu_path)))

    click(bus, name, menu_path, rows[1][0])
    step("the sharing switch pauses sharing", a.wait(lambda s: s["settings"]["sharing"] is False, 5))
    time.sleep(0.3)
    step("... and the first line says so", menu(bus, name, menu_path)[0][1] == "Sharing is paused", menu(bus, name, menu_path)[0][1])

    a.send({"type": "update_settings", "values": {"language": "zh-CN"}})
    a.wait(lambda s: s["settings"]["language"] == "zh-CN", 5)
    time.sleep(0.5)
    chinese = [label for _, label, _, _ in menu(bus, name, menu_path)]
    step("the menu follows the language setting", chinese[-1] != "Quit" and all(ord(max(label)) > 0x2E80 for label in chinese), str(chinese))

    quit_id = menu(bus, name, menu_path)[-1][0]
    click(bus, name, menu_path, quit_id)
    try:
        a.proc.wait(timeout=8)
    except Exception:
        pass
    step("Quit stops the agent", a.proc.poll() == 0, str(a.proc.poll()))
    if a.proc.poll() is None:
        a.proc.kill()
    a.log.close()
    sys.exit(step.finish())
