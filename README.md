# Rinflow for Linux (`rinflow-linux`)

The Linux client of Rinflow: GNOME and KDE on Wayland today, Steam Deck
in both of its modes. It is two programs, as on Windows:

| Program | Role |
| --- | --- |
| `rinflow-agent` | Always running for the signed-in user. Links `rinflow-core` directly, keeps devices connected, captures input through the desktop's `InputCapture` portal when the pointer hits the edge toward a placed device, plays peers' input through libei (the `RemoteDesktop` portal, or gamescope's own EIS socket in Steam Deck's Gaming Mode), and serves a local socket for the window. No GTK, no display required. |
| `rinflow` | The window: GTK 4 + libadwaita. Devices, their arrangement around this screen, pairing, settings. Starts the agent if it is not running; speaks JSON lines to it over `$XDG_RUNTIME_DIR/rinflow/agent.sock`. |

```
 rinflow (GTK4/libadwaita) ── $XDG_RUNTIME_DIR/rinflow/agent.sock, JSON lines ──►  rinflow-agent (Rust)
   devices, arrangement,                                                            ├ rinflow-core ── UDP 42420 / QUIC 42421 ──► other devices
   pairing, settings                                                                ├ capture: InputCapture portal + libei      (GNOME ≥ 45, Plasma ≥ 6.1)
                                                                                    ├ emulation: RemoteDesktop portal + libei   │ gamescope EIS socket
                                                                                    └ logind sleep/wake, network changes
```

The design, the research behind it and what is deliberately left out are in
[docs/LINUX_PLAN.md](../docs/LINUX_PLAN.md).

## Layout

```
Cargo.toml                 workspace: agent, input, shell, app
agent/                     rinflow-agent
  src/main.rs              start-up, the main loop that ties core, backends, driver and IPC together
  src/backends.rs          keeps capture and emulation running: detects the desktop, starts, restarts after loss
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
  src/ei_discover.rs       finding gamescope's `*-ei` socket in the runtime directory
  src/keymap.rs            USB HID usages ↔ evdev key codes, core button numbers ↔ BTN_*
  src/token.rs             the portals' restore tokens (one use each)
app/                       rinflow
  build.rs                 blueprint-compiler → GtkBuilder XML → GResource
  src/main.rs              gettext, theme, resources, actions
  src/agent.rs             socket client: connects, starts the agent, reads snapshots, sends requests
  src/window.rs            the window (composite template from data/ui/window.blp)
  src/canvas.rs            the arrangement canvas: screens to scale, drag to place, snapping preview
data/                      desktop entry, AppStream metainfo, icons, systemd user unit, style.css, ui/*.blp
po/                        translations (zh_CN)
tests/e2e_two_agents.py    end-to-end test through the real core, two agents on one host
tests/e2e_crossing.py      the crossing: barrier hit → entry on the peer → motion, keys, buttons, scroll → return → release
shell/                     rinflow-shell, shared by agent and app: state snapshot, IPC protocol, settings, trust, identity, arrangement
tests/fakedesk/            a stand-in desktop: InputCapture + RemoteDesktop portals on D-Bus, EIS servers via reis, a control socket
tests/e2e_gnome.py         the crossing on two real Mutter (GNOME 46) desktops, headless; a11y_allow.py presses the real permission dialogs,
                           injector (tests/fakedesk/src/bin) moves A's pointer through Mutter's own portal, observer.py records what Mutter B delivers
build-aux/                 Flatpak manifest, the Ubuntu 24.04 build container
```

`rinflow-core` is a path dependency and must be checked out next to this
directory:

```
workspace/
  rinflow-core/
  rinflow-linux/
```

## Building

Rust (stable, edition 2024), GTK 4 ≥ 4.14, libadwaita ≥ 1.5 (Ubuntu 24.04 LTS
and newer, Fedora 40+, Debian 13), `blueprint-compiler`, `gettext`:

```sh
# Ubuntu / Debian
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev blueprint-compiler gettext
# Fedora
sudo dnf install gtk4-devel libadwaita-devel blueprint-compiler gettext

make run        # debug build, starts the window (which starts the agent)
make test       # unit tests of the workspace
make e2e        # two agents on this machine: discovery, pairing, connection, placement sync (no display needed)
make e2e-crossing  # the crossing itself, with tests/fakedesk standing in for the compositors (needs dbus-daemon)
make e2e-gnome     # the crossing on two real GNOME desktops (Mutter headless + GNOME portal backend; see build-aux/Dockerfile.ubuntu-24.04)
make e2e-persist   # portal permission persistence: first start asks, second start is silent (GNOME 51 / Plasma 6.8: build-aux/Dockerfile.fedora-45)
make e2e-controlled  # a real desktop (Mutter or KWin, RINFLOW_TEST_DESKTOP) as the controlled device
make check      # desktop-file-validate + appstreamcli validate
sudo make install && systemctl --user enable --now rinflow-agent
```

Flatpak (the way it reaches Steam Deck and distributions with an older
libadwaita), from the workspace root, offline:

```sh
flatpak install flathub org.gnome.Platform//50 org.gnome.Sdk//50 org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak-builder --user --install --force-clean build-dir rinflow-linux/build-aux/com.rinflow.linux.yml
flatpak run com.rinflow.linux
```

`build-aux/cargo-sources.json` pins every crate; regenerate it after
`Cargo.lock` changes with `flatpak-cargo-generator.py Cargo.lock -o build-aux/cargo-sources.json`
(from flatpak-builder-tools). A `.deb` comes from `cargo install cargo-deb &&
make locale && cargo build --release && cargo deb -p rinflow --no-build`.

Logs: `journalctl --user -u rinflow-agent -f` under systemd, or `RUST_LOG=debug
./target/debug/rinflow-agent` by hand.

## How it behaves on each desktop

| Desktop | Control others | Be controlled | What the user sees |
| --- | --- | --- | --- |
| GNOME ≥ 45, Wayland | yes (InputCapture portal) | yes (RemoteDesktop portal) | A permission dialog from the desktop. Before GNOME 51 the capture one returns every login; the emulation one is remembered from the first time. |
| KDE Plasma ≥ 6.1, Wayland | yes | yes | The same; both remembered from Plasma 6.7 on. |
| Steam Deck, Desktop Mode | yes | yes | Plasma: as above. |
| Steam Deck, Gaming Mode | **no** (gamescope has no capture interface) | yes (gamescope's EIS socket, no prompt) | Nothing; the agent finds the socket by itself. Pair in Desktop Mode first. |
| X11 session | not yet | not yet | The window says so. |

**Status: not finished.** Tested only headless in aarch64 containers, with injected input. Plasma as the controlling side currently fails, and nothing has run on real hardware. See [docs/LINUX_STATUS.md](../docs/LINUX_STATUS.md).

The design is one systemd user service for the whole login, so that switching the Deck between modes does not restart it. Picking up a new compositor is **not implemented yet**: the agent only re-reads its own environment (see the status document).

## Files

| What | Where |
| --- | --- |
| Device ID, identity certificate, paired devices, layout, settings | `~/.local/share/rinflow/` |
| Portal restore tokens, last known displays | `~/.local/state/rinflow/` |
| The window's socket | `$XDG_RUNTIME_DIR/rinflow/agent.sock` |
| Start at login | `~/.config/autostart/com.rinflow.linux.agent.desktop` (outside Flatpak) |

## Not in this version

Clipboard and files, X11 and wlroots backends, the tray icon, the Decky
plugin for Gaming Mode, Hyprland through its portal (it leaks file
descriptors per session in released versions). The order they come in is
in the plan.
