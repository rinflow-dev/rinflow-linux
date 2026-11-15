# Status of the Linux client

First MVP, 0.0.1. What works, how that was checked, and what has not been
checked yet. Update this file when something is verified on a new desktop,
or found broken.

## In this version

- **Devices**: discovery (multicast, broadcast, by address), pairing with the
  six-digit code, encrypted sessions, staying connected, sleep and network
  changes, arranging devices around this screen.
- **Controlling other devices**: the pointer crosses at the edges where a
  device is placed (InputCapture portal + libei); keys, buttons, scrolling,
  key repeat, moving on to the next device in a chain, Ctrl+Alt+Esc.
- **Being controlled**: a peer's input is played through the RemoteDesktop
  portal + libei, or gamescope's EIS socket in Steam Deck's Gaming Mode.
- **Clipboard** (new): text, HTML and PNG follow the pointer both ways,
  through the Clipboard portal of the RemoteDesktop session; on by default;
  a password manager's secrets stay on the device.
- **The desktop coming and going** (new): the user service starts before the
  desktop and finds it through the systemd user manager's environment, and
  lets go of it when its Wayland socket is gone.
- **Barriers that keep working on GNOME** (fixed on a real session):
  Mutter refuses `SetPointerBarriers` while the capture session is enabled,
  and the agent re-checks its barriers on every connection and every change
  of who controls whom, so after the first change the edges stopped working.
  A change now goes Disable → set → Enable, and unchanged barriers are not
  set again. tests/fakedesk refuses as Mutter does; `make e2e-crossing`
  moves a placed device while enabled.
- **Arranging by dragging** (reworked, as on Windows): the view holds still
  while a device moves (it re-fitted on every frame, so the device drifted
  away from the pointer); a click no longer places a device; the agent's
  snap shows as a dashed outline, and close to it the device jumps there
  like a magnet; a dropped device glides to its spot instead of jumping back
  until the agent answers; the user's online devices that aren't placed wait
  as chips on a strip at the bottom, and a placed device dropped there is
  unplaced; Escape or a lost pointer puts a dragged device back; previews
  are asked one at a time. The window also lost requests to the agent when
  two were written at once (GIO takes one write at a time), the drop right
  after a preview among them: they now go out in order.
- **The pairing dialog** (fixed): it came back right after Accept (an
  alert dialog closes on any answer, and the next snapshot, which still
  carries the pairing, opened a new one), blocking the window with only
  Reject until the other device answered or two minutes passed. A pairing
  now asks once; accepted, it waits in a banner whose Cancel declines it.
  The agent logs sessions and pairing (`core connected/disconnected/
  pairing_requested/paired/pairing_ended: <peer> <reason>`), to tell why a
  device didn't pair.
- **Languages** (new): English, Simplified Chinese and Japanese, in the
  window, the agent's own messages, the desktop entry and the AppStream
  metainfo, all from GNU gettext catalogs (`po/`). The agent sends its
  messages untranslated (protocol 2) and the window translates everything
  it shows, so a change of the setting applies to the whole window at once,
  without a restart.
- **Release review fixes** (new): a device ID with a `'` no longer aborts
  the window (menu targets are typed variants, never parsed strings), and
  names from other devices are plain text, not markup. The pointer is
  never left captured: an activation without a position, a refused
  Release, or `Disabled` mid-capture all hand it back and tell the driver;
  Ctrl+Alt+Esc works even when nothing is driven (`CaptureCommand::Escape`).
  A stopped capture backend closes its portal session instead of being
  aborted, and Retry restarts only a backend that isn't working. The key
  repeat tick no longer bursts after idling. One agent at a time (a lock
  before the socket), the user service restarts only on failure, and the
  window starts it through systemd when it runs the same program. A
  pending pairing can't be replaced by another device's request, Enter no
  longer accepts it, and Accept waits 1.5 s after the code is shown.
- **Copied files** (new): files and folders copied in a file manager go
  along with the clipboard, with the clipboard's switch. On the other
  device they are only described; what is pasted there is read from this
  one then (a FUSE file system on Linux, `agent/src/remote_files.rs`),
  with progress and Cancel in the window. Nothing is downloaded by itself.
- **Staying on one screen** (new): a lock (Advanced page, tray, `rinflowctl
  lock`, Scroll Lock while driving), a delay before crossing and a margin
  around screen corners.
- **`rinflowctl`** (new): status, layout, placing devices by side or by
  coordinates, pairing, the lock, settings, from a terminal.
- **Notifications** (new): a pairing request while no window is open, as
  a desktop notification from the agent.
- **Tray icon** (new): a StatusNotifierItem from the agent.
- **When the core cannot start** (new): its port taken, say, the agent stays
  up and the window says why and what to do, instead of the agent exiting.

## Verified

| What | Where | How | Result |
| --- | --- | --- | --- |
| Build: agent and GTK window | Debian 13, x86_64, GTK 4.18, libadwaita 1.7, Rust 1.98 | `cargo build`, `cargo clippy` | builds; no new lints |
| Unit tests | same | `make test` | pass |
| Discovery, pairing, sessions, placement sync | two agents on one host | `make e2e` | pass |
| The crossing, both ways | fake desktops (tests/fakedesk) | `make e2e-crossing` | pass |
| Clipboard both ways, the setting on either side, privacy, no echo | fake desktops, Clipboard portal as xdg-desktop-portal 1.20 sends it | `make e2e-clipboard` | pass |
| The agent's text goes out untranslated whatever its environment; the setting is kept, an unknown language refused | one agent, no display | `make e2e-i18n` | pass |
| Catalog choice as libc makes it (`LANGUAGE` list, the C locale, `zh`/`zh_SG` → `zh_CN`), the setting over it; one-pass placeholders | unit tests | `cargo test -p rinflow-shell` | pass |
| Translations complete and well-formed; the translated desktop entry and metainfo valid | same | `make check` (check-po, desktop-file-validate, appstreamcli) | pass |
| The window in Japanese on a real GNOME 48 session | Debian 13 desktop | by hand, `LANGUAGE=ja`, read through AT-SPI | every label translated |
| The arrangement under a real pointer: a click places nothing, a chip dragged beside the screen is placed, a device dropped on the strip unplaced, Escape puts back, one preview at a time | the window under Xvfb, a stand-in agent | `make e2e-canvas` (XTest through AT-SPI) | pass |
| The pairing dialog: asked once, Accept final under repeated snapshots, the banner's Cancel declines, a new code asks again | the window under Xvfb, a stand-in agent | `make e2e-window-pairing` (AT-SPI) | pass |
| Service started before the desktop; desktop going away | fake desktop + fake systemd manager | `make e2e-systemd` | pass |
| The crossing between two real GNOME desktops | Mutter 48.7 headless + xdg-desktop-portal-gnome 48 + xdg-desktop-portal 1.20, x86_64 | `make e2e-gnome` | pass |
| A real GNOME desktop as the controlled device | same | `make e2e-controlled` | pass |
| Clipboard between two real GNOME desktops: GTK copies, GTK pastes (text with CJK, HTML, PNG pixel-exact) | same | `make e2e-gnome-clipboard` | pass |
| The agent on a real GNOME 48 Wayland session | Debian 13 desktop, one 1920×1080 monitor | by hand | core runs, the portal's display geometry is picked up, the Windows PC and the Mac on the LAN are discovered; GNOME shows its permission dialogs |
| The window on a real GNOME 48 session | same | by hand, read through AT-SPI | opens, shows both permission states and the nearby devices |
| Permissions on a real GNOME 48 session | same | by hand | both dialogs allowed; on the next start RemoteDesktop (and its clipboard) came back silently from the restore token, InputCapture asked again (as expected before GNOME 51) |
| The crossing on a real GNOME 48 session (used over VNC) | same, against a stand-in peer on the same host | relative motion injected through Mutter's RemoteDesktop/EIS (tests/fakedesk `injector`), the path a local mouse takes; VNC sends absolute motion, which barriers ignore | crossing at the right edge, entry point exact, motion and keys arrive, back across the edge, a second crossing and Ctrl+Alt+Esc all pass; the clipboard went both ways (text copied on the real desktop pasted on the peer; the peer's text taken by an application on the real desktop). The first attempt, on an agent that had waited 38 minutes for the dialog, crossed but no captured input reached the agent; not seen again and not explained. Captured EIS events are now logged (`RUST_LOG=rinflow_input=debug`, motion at `trace`) to tell if it comes back |
| A Mac controlling a real GNOME 48 session | Debian 13 desktop, the macOS client on the LAN | by hand | pairing, the Mac's pointer and keyboard here; files copied on the Mac pasted in the file manager here (read on demand through the mount), files copied here pasted on the Mac |
| Re-setting barriers on a real GNOME 48 session | same | by hand, watching the portal on the bus | refused while enabled before the fix; Disable → set → Enable accepted |

Earlier, in aarch64 containers: Mutter 46 (Ubuntu 24.04) for the crossing,
GNOME 51 and Plasma 6.8 (Fedora 45) for permission persistence
(`make e2e-persist`, which needs that container: it reads versions with
`rpm`), gamescope (`tests/e2e_gamescope.py`).

## Not verified yet

- **A physical mouse** on a real session (the session was used over VNC; the
  crossing was driven by relative motion injected through Mutter instead).
- **Interoperation with the Windows client**: pairing, crossing and the
  clipboard with a real PC. (A Mac controlling this device is in the table
  above; this device controlling a Mac is not.)
- **Plasma as the controlling side**: it failed in the earlier container
  runs and has not been looked at since. Plasma as the controlled side
  passed there.
- **Steam Deck hardware**, both modes, and switching between them on the
  device (the logic is covered by `make e2e-systemd` and unit tests).
- **Several monitors and fractional scaling** on real hardware.
- **The Flatpak** build. `build-aux/cargo-sources.json` is behind
  `Cargo.lock` (15 crates, `fuser` among them) and must be regenerated
  before it builds.

## Known limits

- Pasting files copied on another device needs FUSE (`fusermount3`), so it
  does not work from the Flatpak; sending from it needs access to where
  the files are (`--filesystem=home`). A peer that cannot be read on demand
  (an older Rinflow) has its files declined. Dragging files across the edge
  is not done.
- Pasting a peer's files has been run between two Linux agents
  (`make e2e-clipboard`, files read through the mount) and from a Mac into
  a real file manager, not yet with a Windows PC.
- A shortcut cannot send the pointer to another device: the InputCapture
  portal starts a capture only at an edge. It can bring it home.
- The tray icon, notifications, the lock, the crossing delay and copied files have only
  been run against the stand-in desktop (tests/fakedesk) and a stand-in
  tray host, not yet on a real GNOME or Plasma session.
- The clipboard needs the RemoteDesktop permission (it comes with it), so it
  is not there when other devices may not control this one, nor in Gaming
  Mode.
- X11 sessions and wlroots compositors are not supported; Hyprland's portal
  leaks file descriptors per session in released versions.
- GTK's and libadwaita's own strings (the About dialog's tabs, an entry's
  menu) change language the next time the window opens; everything else at
  once. An open dialog keeps the language it was opened in.
- Before GNOME 51 the capture permission is asked at every login (the
  desktop remembers only the RemoteDesktop one).
