"""A full-screen GTK 4 window that copies and pastes through the desktop's
real clipboard, as an application would; driven over a Unix socket, one
JSON object per line:

  {"copy_text": "…"}            → {"ok": true}
  {"copy_html": "…", "text": "…"} → HTML and a plain-text fallback, as browsers copy
  {"copy_png": "<hex>"}         → an image, offered as image/png
  {"formats": true}             → {"formats": [MIME types the clipboard offers]}
  {"paste_text": true}          → {"text": "…"} or {"error": "…"}
  {"paste_png": true}           → {"width": w, "height": h, "pixel": [r, g, b, a]} of the top-left pixel

Usage: clipapp.py <socket path>"""
import json, os, sys
import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk

PATH = sys.argv[1]
window = None


def clipboard():
    return window.get_display().get_clipboard()


def reply(out, value):
    out.write_all(json.dumps(value).encode() + b"\n", None)


def handle(command, out):
    cb = clipboard()
    if "copy_text" in command:
        cb.set(command["copy_text"])
        reply(out, {"ok": True})
    elif "copy_html" in command:
        html = Gdk.ContentProvider.new_for_bytes("text/html", GLib.Bytes.new(command["copy_html"].encode()))
        text = Gdk.ContentProvider.new_for_value(command.get("text", ""))
        cb.set_content(Gdk.ContentProvider.new_union([html, text]))
        reply(out, {"ok": True})
    elif "copy_png" in command:
        # As screenshot tools do: the PNG itself. (gdk_clipboard_set_texture
        # is not introspectable, and a GValue made from Python carries the
        # texture's subclass, which GTK offers under no MIME type at all.)
        cb.set_content(Gdk.ContentProvider.new_for_bytes("image/png", GLib.Bytes.new(bytes.fromhex(command["copy_png"]))))
        reply(out, {"ok": True})
    elif "formats" in command:
        reply(out, {"formats": list(cb.get_formats().get_mime_types() or [])})
    elif "paste_text" in command:
        def done(source, result):
            try:
                reply(out, {"text": source.read_text_finish(result)})
            except GLib.Error as error:
                reply(out, {"error": error.message})
        cb.read_text_async(None, done)
    elif "paste_png" in command:
        def done(source, result):
            try:
                texture = source.read_texture_finish(result)
            except GLib.Error as error:
                reply(out, {"error": error.message})
                return
            if texture is None:
                reply(out, {"error": "no image"})
                return
            data = texture.save_to_png_bytes().get_data()
            again = Gdk.Texture.new_from_bytes(GLib.Bytes.new(data))
            downloader = Gdk.TextureDownloader.new(again)
            downloader.set_format(Gdk.MemoryFormat.R8G8B8A8)
            pixels, _stride = downloader.download_bytes()
            reply(out, {"width": texture.get_width(), "height": texture.get_height(), "pixel": list(pixels.get_data()[:4])})
        cb.read_texture_async(None, done)
    else:
        reply(out, {"error": "unknown command"})


def serve(connection):
    data = Gio.DataInputStream.new(connection.get_input_stream())
    out = connection.get_output_stream()

    def got_line(stream, result):
        try:
            line, _ = stream.read_line_finish_utf8(result)
        except GLib.Error:
            return
        if line is None:
            return
        try:
            handle(json.loads(line), out)
        except Exception as error:  # noqa: BLE001 — a test helper reports everything
            reply(out, {"error": str(error)})
        stream.read_line_async(GLib.PRIORITY_DEFAULT, None, got_line)

    data.read_line_async(GLib.PRIORITY_DEFAULT, None, got_line)
    serve.keep.append((connection, data))


serve.keep = []


def on_activate(app):
    global window
    window = Gtk.ApplicationWindow(application=app, title="clipapp")
    label = Gtk.Label(label="clipapp")
    label.set_focusable(True)
    window.set_child(label)
    window.fullscreen()
    window.present()
    label.grab_focus()
    try:
        os.unlink(PATH)
    except FileNotFoundError:
        pass
    service = Gio.SocketService.new()
    service.add_address(Gio.UnixSocketAddress.new(PATH), Gio.SocketType.STREAM, Gio.SocketProtocol.DEFAULT, None)
    service.connect("incoming", lambda s, connection, source: serve(connection) or False)
    service.start()
    on_activate.service = service


app = Gtk.Application(application_id="com.rinflow.test.clipapp", flags=Gio.ApplicationFlags.NON_UNIQUE)
app.connect("activate", on_activate)
app.run([])
