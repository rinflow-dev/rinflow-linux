# Design

The Linux client of Rinflow: GNOME and KDE on Wayland today, Steam Deck
in both of its modes. It is two programs, as on Windows:

| Program | Role |
| --- | --- |
| `rinflow-agent` | Always running for the signed-in user. Links `rinflow-core` directly, keeps devices connected, captures input through the desktop's `InputCapture` portal when the pointer hits the edge toward a placed device, plays peers' input through libei (the `RemoteDesktop` portal, or gamescope's own EIS socket in Steam Deck's Gaming Mode), shares the clipboard through the `Clipboard` portal of that same session, and serves a local socket for the window. No GTK, no display required. |
| `rinflow` | The window: GTK 4 + libadwaita. Devices, their arrangement around this screen, pairing, settings. Starts the agent if it is not running; speaks JSON lines to it over `$XDG_RUNTIME_DIR/rinflow/agent.sock`. |

```
 rinflow (GTK4/libadwaita) ── $XDG_RUNTIME_DIR/rinflow/agent.sock, JSON lines ──►  rinflow-agent (Rust)
   devices, arrangement,                                                            ├ rinflow-core ── UDP 42420 / QUIC 42421 ──► other devices
   pairing, settings                                                                ├ capture: InputCapture portal + libei      (GNOME ≥ 45, Plasma ≥ 6.1)
                                                                                    ├ emulation: RemoteDesktop portal + libei   │ gamescope EIS socket
                                                                                    ├ clipboard: Clipboard portal on that RemoteDesktop session
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
  src/desktop.rs           which desktop is up: the agent's own environment, or the systemd user manager's
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
                           optionally the systemd user manager's environment; a control socket
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
| X11 session | not yet | not yet | The window says so. |

## Clipboard

**Share the clipboard** on the Settings page (on by default).
The clipboard follows the pointer, as in Deskflow: when this device starts
driving another, that device gets this one's clipboard, and when control
comes back, its clipboard comes back. Text, HTML and PNG images, up to 8 MiB
in all; files are not shared yet.

| | How |
| --- | --- |
| Access | The `Clipboard` portal, asked for with the emulation backend's `RemoteDesktop` session and granted with it (GNOME ≥ 45, Plasma ≥ 6.2). So the clipboard is there when other devices may control this one; the switch says so otherwise. Not in Gaming Mode (gamescope has no clipboard interface). |
| Copied here | When another application takes the clipboard, the shared formats are read at once and handed to the core, which sends them when control changes hands. While the setting is off nothing copied is read at all, and peers get an empty clipboard. |
| From a peer | Announced as this session's selection; an application that pastes gets the data from the agent (`SelectionTransfer`). Text is offered under `text/plain;charset=utf-8`, `UTF8_STRING`, `text/plain`, `STRING` and `TEXT`, so GTK, Qt and X11 programs all find it. |
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

While no window is open the agent says what needs the user with a desktop
notification (`org.freedesktop.Notifications`, under Rinflow's desktop
entry, so the desktop's settings can silence it):

| | |
| --- | --- |
| A device asks to pair | Click to open the window and compare the code, or **Reject**. It cannot be accepted from the notification, and the code is not shown there. Withdrawn when the pairing is over or a window opens; the same device asking again within 30 seconds is not announced twice. |

A window that is open asks by itself (and comes forward), so nothing is
announced twice.

## Not in this version

X11 and wlroots backends, dragging files across the edge, the Decky
plugin for Gaming Mode, Hyprland through its portal (it leaks file
descriptors per session in released versions).
