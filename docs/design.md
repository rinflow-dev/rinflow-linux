# Design

The Linux client of Rinflow: GNOME and KDE on Wayland, X11 sessions
(Cinnamon, Xfce, MATE, a window manager by itself), Steam Deck in both of
its modes. It is two programs, as on Windows:

| Program | Role |
| --- | --- |
| `rinflow-agent` | Always running for the signed-in user. Links `rinflow-core` directly, keeps devices connected, captures input when the pointer hits the edge toward a placed device, plays peers' input, shares the clipboard, and serves a local socket for the window. On Wayland that is the desktop's `InputCapture` portal, libei (the `RemoteDesktop` portal, or gamescope's own EIS socket in Steam Deck's Gaming Mode) and the `Clipboard` portal of that same session; on X11 the X server's own extensions (XFixes barriers, XInput raw events, XTest) and the CLIPBOARD selection. No GTK, no display required. |
| `rinflow` | The window: GTK 4 + libadwaita. Devices, their arrangement around this screen, pairing, settings. Starts the agent if it is not running; speaks JSON lines to it over `$XDG_RUNTIME_DIR/rinflow/agent.sock`. |

```
 rinflow (GTK4/libadwaita) ── $XDG_RUNTIME_DIR/rinflow/agent.sock, JSON lines ──►  rinflow-agent (Rust)
   devices, arrangement,                                                            ├ rinflow-core ── UDP 42420 / QUIC 42421 ──► other devices
   pairing, settings                                                                ├ capture: InputCapture portal + libei      (GNOME ≥ 45, Plasma ≥ 6.1)   │ X11: XFixes barriers + XInput raw events
                                                                                    ├ emulation: RemoteDesktop portal + libei   │ gamescope EIS socket         │ X11: XTest
                                                                                    ├ clipboard: Clipboard portal on that RemoteDesktop session              │ X11: the CLIPBOARD selection
                                                                                    └ logind sleep/wake, network changes, the desktop coming and going
```

What has been verified, where, and what has not is in
[status.md](status.md). How each part works is described in the
comments at the top of each module.

## Layout

```
Cargo.toml                 workspace: agent, input, shell, app
agent/                     rinflow-agent
  src/main.rs              start-up, the main loop that ties core, backends, driver and IPC together
  src/backends.rs          keeps capture and emulation running: starts, restarts after loss, follows the desktop
  src/desktop.rs           which desktop is up, and its X server on X11: the agent's own environment, or the systemd user manager's
  src/driver.rs            this device driving a peer: the core's RemotePointer, keys, buttons, key repeat, Ctrl+Alt+Esc
  src/ipc.rs               the window's socket (rinflow-shell's protocol)
  src/services.rs          what the window may ask for
  src/power.rs             logind PrepareForSleep; address changes by polling getifaddrs
  src/displays.rs          last known displays, so the core starts with geometry before the portal answers
  src/autostart.rs         ~/.config/autostart entry ("Start when I sign in")
  src/paths.rs             XDG directories, app ID, host name
input/                     rinflow-input: the backends, no agent logic
  src/portal_capture.rs    InputCapture portal + libei receiver; one session for the backend's life
  src/ei_emulation.rs      libei sender; fd from the RemoteDesktop portal or a compositor socket
  src/portal_clipboard.rs  the Clipboard portal on the emulation session: reads what is copied, offers a peer's clipboard
  src/clipboard_formats.rs which formats are shared and what applications call them, for both clipboards
  src/x11_capture.rs       X11: pointer barriers (XFixes), the hit and what the mouse and keyboard do then (XInput raw events), grabs
  src/x11_emulation.rs     X11: a peer's input played through XTest
  src/x11_clipboard.rs     X11: the CLIPBOARD selection, by the ICCCM conventions (TARGETS, MULTIPLE, INCR)
  src/x11_common.rs        X11: the connection (the session's DISPLAY and XAUTHORITY), its events on the agent's loop, points and pixels
  src/ei_discover.rs       finding gamescope's `*-ei` socket in the runtime directory
  src/keymap.rs            USB HID usages ↔ evdev key codes, core button numbers ↔ BTN_*
  src/token.rs             the portals' restore tokens (one use each)
app/                       rinflow
  build.rs                 blueprint-compiler → GtkBuilder XML → GResource
  src/main.rs              locale and translations, theme, resources, actions
  src/agent.rs             socket client: connects, starts the agent, reads snapshots, sends requests
  src/window.rs            the window (composite template from data/ui/window.blp)
  src/canvas.rs            the arrangement canvas: screens to scale, drag to place, snapping preview
data/                      desktop entry and AppStream metainfo (*.in: translated by msgfmt into target/data), icons, systemd user unit, style.css, ui/*.blp
po/                        translations: LINGUAS (ja, zh_CN), POTFILES, rinflow.pot, <language>.po
shell/src/i18n.rs          translations: the catalogs read in-process (the window's language changes at once), `Text` for the agent's untranslated messages
tests/e2e_two_agents.py    end-to-end test through the real core, two agents on one host
tests/e2e_crossing.py      the crossing: barrier hit → entry on the peer → motion, keys, buttons, scroll → return → release
tests/e2e_x11.py           the same crossing and the clipboard on X11: two agents on two X servers (Xvfb), tests/fakedesk's xdesk as the
                           user and the application, clipapp.py (GTK) copying and pasting, a desktop at 200 % (make e2e-x11)
tests/e2e_clipboard.py     the clipboard following the pointer both ways, the setting, password managers' secrets kept home
tests/e2e_systemd.py       an agent started before the desktop (the user service) finds it through systemd, and lets go of it
tests/e2e_i18n.py          the agent's text goes out untranslated whatever its environment; the language setting is kept and checked
tests/e2e_window_i18n.py   the window changes language while it runs, read through AT-SPI (make e2e-window-i18n)
tests/e2e_canvas.py        the arrangement canvas under a real pointer, against a stand-in agent (make e2e-canvas)
tests/e2e_window_pairing.py  the pairing dialog: asked once, an accepted pairing waits in a banner (make e2e-window-pairing)
tests/check_po.py          every language complete, placeholders intact (make check-po)
tests/e2e_gnome_clipboard.py  the clipboard on two real Mutter desktops: GTK copies on one, GTK pastes on the other (clipapp.py)
shell/                     rinflow-shell, shared by agent and app: state snapshot, IPC protocol, settings, trust, identity, arrangement
tests/fakedesk/            a stand-in desktop: InputCapture, RemoteDesktop and Clipboard portals on D-Bus, EIS servers via reis,
                           optionally the systemd user manager's environment; a control socket. Its xdesk is a stand-in user
                           (XTest) and application (a window that records what it is given) for an X server
tests/e2e_gnome.py         the crossing on two real Mutter (GNOME 46) desktops, headless; a11y_allow.py presses the real permission dialogs,
                           injector (tests/fakedesk/src/bin) moves A's pointer through Mutter's own portal, observer.py records what Mutter B delivers
build-aux/                 Flatpak manifest, the Ubuntu 24.04 build container
```

`rinflow-core` is a git dependency pinned to one commit in `Cargo.toml`, so
this repository builds by itself. To work on the core at the same time,
check it out next to this directory and point Cargo at it in an untracked
`.cargo/config.toml`:

```toml
[patch."https://github.com/rinflow-dev/rinflow-core"]
rinflow-core = { path = "../rinflow-core" }
```

## How it behaves on each desktop

| Desktop | Control others | Be controlled | What the user sees |
| --- | --- | --- | --- |
| GNOME ≥ 45, Wayland | yes (InputCapture portal) | yes (RemoteDesktop portal) | A permission dialog from the desktop. Before GNOME 51 the capture one returns every login; the emulation one is remembered from the first time. |
| KDE Plasma ≥ 6.1, Wayland | yes | yes | The same; both remembered from Plasma 6.7 on. |
| Steam Deck, Desktop Mode | yes | yes | Plasma: as above. |
| Steam Deck, Gaming Mode | **no** (gamescope has no capture interface) | yes (gamescope's EIS socket, no prompt) | Nothing; the agent finds the socket by itself. Pair in Desktop Mode first. |
| X11 session (Cinnamon, Xfce, MATE, i3…) | yes (XFixes pointer barriers, XInput raw events) | yes (XTest) | Nothing: an X server lets any program do this. While another device is controlled the pointer here is hidden. |

## X11

The X server gives what the portals give on Wayland, without asking.

| | How |
| --- | --- |
| Edges | An XFixes pointer barrier on each line where a placed device touches this screen. The server stops the pointer there and reports it with the point (XInput 2.3 `BarrierHit`); nothing polls the pointer. Needs XFixes 5 and XInput 2.3 (X.Org 1.14, 2013). |
| The hit | The pointer is grabbed and kept where it stopped, and its motion is read from XInput raw events, which tell what the mouse did whatever the pointer on screen can do. The keyboard is not touched yet: taking it makes the application in use close its menus, and an edge may lead nowhere just now (the device is off, the corner margin, the delay before crossing). |
| The crossing | When the pointer drives the other device (`CaptureCommand::Engage`), the keyboard is grabbed as well and the pointer hidden; keys and buttons come as raw events. Raw events are selected only from the hit to the release, so the agent sees no key of a desktop it is not capturing. |
| Refused | While another program holds the pointer (an open menu, a drag in progress) the hit is ignored; the next push against the edge tries again. |
| Being controlled | XTest. Keys go by position (the X key code is the evdev code plus eight), so this desktop's layout decides what they type. Played input goes on through another program's server grab (`XTestGrabControl`), and a screen that power management turned off is turned on when the pointer enters. |
| Scrolling | X has wheel buttons and no scroll axis to play into: smooth scrolling from a touchpad arrives as whole notches, and a touchpad here scrolls the other device by notches. |
| Scale | X has one scale for the desktop, which the desktop publishes for toolkits in one of two places: GTK's window scale in XSETTINGS (`Gdk/WindowScalingFactor`; Xfce sets only this, Cinnamon this and the next) and `Xft.dpi` in the resource database (192 is 200 %; Plasma, or set by hand). The larger of the two: at 200 % a 3840×2160 screen is 1920×1080 points. Followed while the agent runs: a change of either, or of the monitors, gives new displays and barriers. |
| Which server | `DISPLAY` and `XAUTHORITY` from the agent's own environment, or the systemd user manager's while that server's socket is there (the user service starts before the session). A Wayland session's `DISPLAY` is for its X programs and is never taken for the desktop. Distributions' X session scripts tell the manager; where nothing has (a session started by hand, or one without the user bus), the window does when it opens, if the manager knows no display at all (`lend_session_to_systemd` in app/src/agent.rs). |

Deskflow's X11 code was read first (`XWindowsComputer.cpp`,
`XWindowsClipboard.cpp`). Taken from it: a window of our own for the
grabs, `XTestGrabControl`, waking the screen, the wheel as buttons, the
selection conventions that matter (`TARGETS` with the wrong type accepted,
`TIMESTAMP`, `MULTIPLE`, `INCR` both ways). Different here: Deskflow
watches the pointer's position and warps it to the middle of the screen to
measure motion, discarding its own warps by marker events; barriers and
raw events need neither. It sends key symbols, Rinflow key positions, so
there is no keyboard map to search.

## Clipboard

**Share the clipboard** on the Settings page (on by default).
The clipboard follows the pointer, as in Deskflow: when this device starts
driving another, that device gets this one's clipboard, and when control
comes back, its clipboard comes back. Text, HTML and PNG images, up to 8 MiB
in all; files are not shared yet.

| | How |
| --- | --- |
| Access | On Wayland the `Clipboard` portal, asked for with the emulation backend's `RemoteDesktop` session and granted with it (GNOME ≥ 45, Plasma ≥ 6.2). So the clipboard is there when other devices may control this one; the switch says so otherwise. Not in Gaming Mode (gamescope has no clipboard interface). On X11 the CLIPBOARD selection, which any program may use: always there. |
| Copied here | When another application takes the clipboard (the portal's `SelectionOwnerChanged`; on X11 the XFixes selection notification), the shared formats are read at once and handed to the core, which sends them when control changes hands. While the setting is off nothing copied is read at all, and peers get an empty clipboard. |
| From a peer | Announced as this session's selection; an application that pastes gets the data from the agent (`SelectionTransfer`; on X11 a `SelectionRequest`, in pieces by `INCR` when it is large). Text is offered under `text/plain;charset=utf-8`, `UTF8_STRING`, `text/plain`, `STRING` and `TEXT`, so GTK, Qt and older X11 programs all find it. |
| Privacy | What a password manager marks with `x-kde-passwordManagerHint: secret` (KeePassXC, KWallet) never leaves this device. |
| Quirk handled | xdg-desktop-portal 1.20 sends `SelectionOwnerChanged`'s `mime_types` wrapped in a struct, `(as)`, where the specification has `as`; ashpd drops such signals silently, so the agent reads them off the bus itself and accepts both. |

**Status: first release, 0.0.1.** On GNOME 48 (Debian 13, x86_64) the crossing, being controlled and the clipboard pass end to end between real Mutter desktops (headless), and a Mac has controlled a real GNOME session, clipboard and copied files included. Plasma as the controlling side is not verified (it failed in earlier container runs), and nothing has been run against the Windows client yet. See [status.md](status.md).

The design is one systemd user service for the whole login, so that switching the Deck between modes does not restart it. A service starts before any desktop, so the agent reads the desktop's variables from the systemd user manager as well as from its own environment (desktops push them there with `import-environment`), and follows the desktop as it comes and goes: a Wayland display counts only while its socket is there.

## Where things are kept

| What | Where |
| --- | --- |
| Device ID, identity certificate, paired devices, layout, settings | `~/.local/share/rinflow/` |
| Portal restore tokens, last known displays | `~/.local/state/rinflow/` |
| The window's socket | `$XDG_RUNTIME_DIR/rinflow/agent.sock` |
| Start at login | `~/.config/autostart/com.rinflow.rinflow.agent.desktop` (outside Flatpak) |

## Copied files

Copied files go with the clipboard (**Share the clipboard** on the Settings
page; there is no separate switch). Files and folders copied in a file manager on one device
can be pasted on the next once the pointer has moved there.

| | How |
| --- | --- |
| Copied here | The file manager's `text/uri-list` (or `x-special/gnome-copied-files`) is read with the clipboard and handed to the core, which sends a manifest; the paths stay on this device and the contents go when the other device takes them. While the setting is off the list is not read. |
| From a peer | The peer only describes its files (names and sizes). They appear, without contents, in a read-only FUSE file system at `$XDG_RUNTIME_DIR/rinflow/files`, and go on this desktop's clipboard as `text/uri-list` and `x-special/gnome-copied-files`, so Nautilus, Dolphin and the rest paste them. What the file manager reads when the user pastes is fetched from the peer then, a piece at a time, with a progress banner in the window that can cancel it. Nothing is transferred for a copy that is never pasted, and nothing is kept but what was pasted, where it was pasted. Needs `fusermount3` (the `fuse3` package). |
| What a pasted file gets | Its contents and name; the mode of a file made here (`0644`, folders `0755`), since file managers copy the mode with the contents and the mount's own read-only bits would leave pasted files unwritable. The peer's own permissions are not on the wire: an executable script arrives without its execute bit. Links are not followed and not sent. |
| Cancel | **Cancel** in the window's banner (or turning sharing off) makes the reads fail with `ECANCELED`, which GIO takes for a cancelled copy: Nemo and Nautilus stop as for their own Stop, Thunar asks whether to skip ("Operation canceled"). Anything else that ends a paste (the peer gone, a timeout, a file gone there) is `EIO`, and the file manager says the copy failed. Either way the file manager keeps the part it wrote, as it does when stopped itself. |
| Limits | One copy holds at most 10,000 files and folders, 32 levels deep (the core's). Past them nothing is offered, and the user hears it at once, as on Windows: a desktop notification ("Can't share the copied files … Zip them into one file, then copy that"), and the same in the window. |
| Older peers | A peer that cannot be read on demand would have its files downloaded at once: that is declined, and the window says to update Rinflow there. |
| Flatpak | A sandbox cannot mount FUSE, so files copied on other devices cannot be pasted from the Flatpak. Sending reads the copied files where they are, which the sandbox allows only for what it can see: `flatpak override --user --filesystem=home com.rinflow.rinflow`. |

## Staying on one screen

| | |
| --- | --- |
| **Keep the pointer on this screen** | On the Advanced page, the tray menu, or `rinflowctl lock [on\|off\|toggle]`. At home the edges stop leading anywhere; while another device is controlled the pointer stays on it. Not kept between runs. |
| Scroll Lock | While another device is controlled, Scroll Lock does the same (it is not sent on); a setting turns that off. |
| Shortcuts | The InputCapture portal starts a capture only at an edge, so a key cannot send the pointer *to* another device. Coming back can be a key: bind `rinflowctl home` (or `rinflowctl lock toggle`) in the desktop's keyboard shortcuts. Ctrl+Alt+Esc always comes home. |
| **Delay before crossing** | The pointer crosses only after pushing against the edge for this long (0–2000 ms); stopping or pulling back gives it back. |
| **Corner margin** | Edge hits this close to a screen corner (0–400 points) do not cross: hot corners and menus live there. |

## The command line

`rinflowctl` speaks the window's protocol to the agent: for machines without
a display, scripts and keyboard shortcuts.

```sh
rinflowctl status              # this device, its devices, input, the lock
rinflowctl layout              # displays and placed devices, in logical points
rinflowctl place mac left      # attach a device to a side: left, right, above, below
rinflowctl place mac --at 1920,120   # or give its top-left corner; the core snaps it
rinflowctl unplace mac
rinflowctl connect mac; rinflowctl pair accept
rinflowctl lock toggle; rinflowctl home
rinflowctl set switch_delay_ms 250
```

A device is named by its name or ID, or the start of either.

## The tray icon

A StatusNotifierItem from the agent (no toolkit): what is going on, pause
sharing, keep the pointer here, open the window, quit. Plasma, Steam Deck's
Desktop Mode, Cinnamon and Xfce show it; GNOME does with the AppIndicator
extension (`gnome-extensions enable ubuntu-appindicators@ubuntu.com`).

## Notifications

The agent says what needs the user with a desktop notification
(`org.freedesktop.Notifications`, under Rinflow's desktop entry, so the
desktop's settings can silence it):

| | |
| --- | --- |
| A device asks to pair | While no window is open. Click to open the window and compare the code, or **Reject**. It cannot be accepted from the notification, and the code is not shown there. Withdrawn when the pairing is over or a window opens; the same device asking again within 30 seconds is not announced twice. |
| Copied files can't be shared | Whether a window is open or not, right when they are copied (more than 10,000, or nested more than 32 levels deep): what to do instead. Goes away by itself; the window says the same. |

A window that is open asks by itself (and comes forward), so nothing is
announced twice.

## Not in this version

A wlroots backend, dragging files across the edge, the Decky plugin for
Gaming Mode, Hyprland through its portal (it leaks file descriptors per
session in released versions).
