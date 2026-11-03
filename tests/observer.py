"""A full-screen GTK 4 window that writes every pointer motion, button, scroll
and key it receives to a file, one JSON object per line: what the compositor
actually delivered to an application. Usage: observer.py <log file>"""
import json, sys, time
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, GLib

log = open(sys.argv[1], "a", buffering=1)


def write(**kw):
    kw["t"] = time.time()
    log.write(json.dumps(kw) + "\n")


def on_activate(app):
    window = Gtk.ApplicationWindow(application=app, title="observer")
    window.set_default_size(1920, 1080)
    area = Gtk.Label(label="observer")
    area.set_focusable(True)
    window.set_child(area)
    motion = Gtk.EventControllerMotion()
    motion.connect("motion", lambda c, x, y: write(type="motion", x=x, y=y))
    motion.connect("enter", lambda c, x, y: write(type="enter", x=x, y=y))
    motion.connect("leave", lambda c: write(type="leave"))
    window.add_controller(motion)
    click = Gtk.GestureClick()
    click.set_button(0)
    click.connect("pressed", lambda g, n, x, y: write(type="button", button=g.get_current_button(), down=True, x=x, y=y))
    click.connect("released", lambda g, n, x, y: write(type="button", button=g.get_current_button(), down=False, x=x, y=y))
    window.add_controller(click)
    scroll = Gtk.EventControllerScroll()
    scroll.set_flags(Gtk.EventControllerScrollFlags.BOTH_AXES)
    scroll.connect("scroll", lambda c, dx, dy: write(type="scroll", dx=dx, dy=dy) or False)
    window.add_controller(scroll)
    key = Gtk.EventControllerKey()
    key.connect("key-pressed", lambda c, keyval, keycode, state: write(type="key", keycode=keycode, keyval=keyval, down=True) or False)
    key.connect("key-released", lambda c, keyval, keycode, state: write(type="key", keycode=keycode, keyval=keyval, down=False))
    window.add_controller(key)
    window.fullscreen()
    window.present()
    area.grab_focus()
    write(type="ready")


app = Gtk.Application(application_id="com.rinflow.test.observer")
app.connect("activate", on_activate)
app.run([])
