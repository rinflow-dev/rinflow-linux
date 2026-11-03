# Status of the Linux client

First MVP, 0.1.0. What works, how that was checked, and what has not been
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
  through the Clipboard portal of the RemoteDesktop session; off by default;
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
- **The arrangement's shelf** (fixed): only devices on the network now wait
  there, in rows. It listed every device ever seen in one row, which shrank
  the arrangement to a sliver and made a small drag land far off.
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
| Service started before the desktop; desktop going away | fake desktop + fake systemd manager | `make e2e-systemd` | pass |
| The crossing between two real GNOME desktops | Mutter 48.7 headless + xdg-desktop-portal-gnome 48 + xdg-desktop-portal 1.20, x86_64 | `make e2e-gnome` | pass |
| A real GNOME desktop as the controlled device | same | `make e2e-controlled` | pass |
| Clipboard between two real GNOME desktops: GTK copies, GTK pastes (text with CJK, HTML, PNG pixel-exact) | same | `make e2e-gnome-clipboard` | pass |
| The agent on a real GNOME 48 Wayland session | Debian 13 desktop, one 1920×1080 monitor | by hand | core runs, the portal's display geometry is picked up, the Windows PC and the Mac on the LAN are discovered; GNOME shows its permission dialogs |
| The window on a real GNOME 48 session | same | by hand, read through AT-SPI | opens, shows both permission states and the nearby devices |
| Permissions on a real GNOME 48 session | same | by hand | both dialogs allowed; on the next start RemoteDesktop (and its clipboard) came back silently from the restore token, InputCapture asked again (as expected before GNOME 51) |
| The crossing on a real GNOME 48 session (used over VNC) | same, against a stand-in peer on the same host | relative motion injected through Mutter's RemoteDesktop/EIS (tests/fakedesk `injector`), the path a local mouse takes; VNC sends absolute motion, which barriers ignore | crossing at the right edge, entry point exact, motion and keys arrive, back across the edge, a second crossing and Ctrl+Alt+Esc all pass; the clipboard went both ways (text copied on the real desktop pasted on the peer; the peer's text taken by an application on the real desktop). The first attempt, on an agent that had waited 38 minutes for the dialog, crossed but no captured input reached the agent; not seen again and not explained. Captured EIS events are now logged (`RUST_LOG=rinflow_input=debug`, motion at `trace`) to tell if it comes back |
| Re-setting barriers on a real GNOME 48 session | same | by hand, watching the portal on the bus | refused while enabled before the fix; Disable → set → Enable accepted |

Earlier, in aarch64 containers: Mutter 46 (Ubuntu 24.04) for the crossing,
GNOME 51 and Plasma 6.8 (Fedora 45) for permission persistence
(`make e2e-persist`, which needs that container: it reads versions with
`rpm`), gamescope (`tests/e2e_gamescope.py`).

## Not verified yet

- **A physical mouse** on a real session (the session was used over VNC; the
  crossing was driven by relative motion injected through Mutter instead).
- **Interoperation with the Windows and macOS clients**: pairing, crossing
  and the clipboard with a real PC and Mac.
- **Plasma as the controlling side**: it failed in the earlier container
  runs and has not been looked at since. Plasma as the controlled side
  passed there.
- **Steam Deck hardware**, both modes, and switching between them on the
  device (the logic is covered by `make e2e-systemd` and unit tests).
- **Several monitors and fractional scaling** on real hardware.
- **The Flatpak** build (`Cargo.lock` gained no new crates, so
  `build-aux/cargo-sources.json` still matches).

## Known limits

- Files are not shared: copied files stay on their device (the core can do
  it; the Linux shell does not offer it yet).
- The clipboard needs the RemoteDesktop permission (it comes with it), so it
  is not there when other devices may not control this one, nor in Gaming
  Mode.
- X11 sessions and wlroots compositors are not supported; Hyprland's portal
  leaks file descriptors per session in released versions.
- No tray icon.
- Before GNOME 51 the capture permission is asked at every login (the
  desktop remembers only the RemoteDesktop one).
