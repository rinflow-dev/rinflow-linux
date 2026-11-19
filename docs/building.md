# Building and running

Rust (stable, edition 2024), GTK 4 ≥ 4.14, libadwaita ≥ 1.5 (Ubuntu 24.04 LTS
and newer, Fedora 40+, Debian 13), `blueprint-compiler`, `gettext`:

```sh
# Ubuntu / Debian
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev blueprint-compiler gettext
# Fedora
sudo dnf install gtk4-devel libadwaita-devel blueprint-compiler gettext

make            # = make run: debug build, closes any Rinflow already running (window, agent, user service), starts the window (which starts the agent)
make run-agent  # the agent alone with its debug log; then ./target/debug/rinflow from another terminal for the window
make stop       # closes every rinflow / rinflow-agent of this user
make test       # unit tests of the workspace
make e2e        # two agents on this machine: discovery, pairing, connection, placement sync (no display needed)
make e2e-crossing  # the crossing itself, with tests/fakedesk standing in for the compositors (needs dbus-daemon)
make e2e-clipboard # the clipboard both ways through the fake Clipboard portal (needs dbus-daemon)
make e2e-x11       # the crossing and the clipboard on X11: two agents on two Xvfb servers, a GTK application copying and pasting,
                   # a desktop at 200 % (needs Xvfb, xrdb, dbus-daemon, python3-gi, gir1.2-gtk-4.0)
make e2e-systemd   # the user service started before the desktop picks it up through systemd (needs dbus-daemon)
make e2e-gnome     # the crossing on two real GNOME desktops (Mutter headless + GNOME portal backend; see build-aux/Dockerfile.ubuntu-24.04)
make e2e-gnome-clipboard  # the clipboard between two real GNOME desktops, GTK copying and pasting
make e2e-persist   # portal permission persistence: first start asks, second start is silent (GNOME 51 / Plasma 6.8: build-aux/Dockerfile.fedora-45)
make e2e-controlled  # a real desktop (Mutter or KWin, RINFLOW_TEST_DESKTOP) as the controlled device
# The e2e-gnome family on a desktop machine also needs the `mutter` binary (Debian/Ubuntu: apt install mutter).
# The tests use their own ports and directories, so they run beside an installed Rinflow; only `make e2e`
# checks the default port 42421 and needs it free.
make e2e-tray     # the tray icon against a stand-in StatusNotifierWatcher: menu read and pressed over D-Bus
make e2e-notify   # notifications against a stand-in notification service: a pairing request announced, opened, declined, withdrawn
make e2e-i18n     # the agent's text goes out untranslated; the language setting (no display needed)
make e2e-window-i18n  # the window switching language live, under Xvfb (AT-SPI)
make e2e-canvas   # dragging in the arrangement: click, strip, unplace, Escape, magnet and previews (Xvfb, XTest)
make e2e-window-pairing  # the pairing dialog asks once; accepted, it waits in a banner (Xvfb, AT-SPI)
make check      # translations complete (check-po) + desktop-file-validate + appstreamcli validate on the translated files
sudo make install && systemctl --user enable --now rinflow-agent
```

Flatpak (the way it reaches Steam Deck and distributions with an older
libadwaita), from the workspace root, offline:

```sh
flatpak install flathub org.gnome.Platform//50 org.gnome.Sdk//50 org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak-builder --user --install --force-clean build-dir rinflow-linux/build-aux/com.rinflow.rinflow.yml
flatpak run com.rinflow.rinflow
```

`build-aux/cargo-sources.json` pins every crate; regenerate it after
`Cargo.lock` changes with `flatpak-cargo-generator.py Cargo.lock -o build-aux/cargo-sources.json`
(from flatpak-builder-tools). A `.deb` comes from `make deb`
(needs `cargo install cargo-deb`) and lands in `target/debian/`. The released
packages, x86_64 and arm64, are built by `.github/workflows/release.yml` when a
version tag is pushed.

## Translations

GNU gettext, one domain (`rinflow`). English is the source; Simplified
Chinese (`zh_CN`) and Japanese (`ja`) are complete. The `Language` setting
chooses one (`system`, `zh-CN`, `ja-JP`, `en-US`); otherwise the system's
language list applies as libc reads it (`LANGUAGE`, then the messages'
locale libc chose from `LC_ALL`, `LC_MESSAGES`, `LANG`; `zh`, `zh_SG` and
`zh_Hans` use the `zh_CN` catalog).

Only the window translates. The agent sends its text untranslated, as a
message id with its values (`Text` in shell/src/i18n.rs; protocol 2), and the
window translates it when it shows it. The window reads the catalogs itself
rather than through libc, so a change of the setting applies at once to
everything on screen, the agent's messages included, without a restart. Only
GTK's and libadwaita's own few strings (the About dialog's tabs, an entry's
menu) come from libc and follow from the next start.

```sh
make pot        # after changing strings: refresh po/rinflow.pot, merge into each po/<language>.po
make check-po   # nothing untranslated or fuzzy, the same {placeholders} as the English
make locale     # target/locale/<language>/LC_MESSAGES/rinflow.mo, target/data/*.desktop, *.metainfo.xml
LANGUAGE=ja ./target/debug/rinflow   # try a language without changing the setting
```

Marking strings: `tr("…")` or `trf("…", &[("name", &value)])` where text is
shown (the window), `msg("…")` with `.arg("name", value)` where it is sent
untranslated (the agent, the settings' errors), `_("…")` in Blueprint. Whole sentences with `{name}` placeholders,
never pieces joined in code; a `// Translators:` comment just above says
what a placeholder holds. A new language: add it to `po/LINGUAS`, `msginit
-i po/rinflow.pot -l <language> -o po/<language>.po`, translate, add it to
`LANGUAGES` in shell/src/i18n.rs (and the list in app/src/window.rs) and to the
`.deb` assets in app/Cargo.toml.

Logs: `journalctl --user -u rinflow-agent -f` under systemd, or `RUST_LOG=debug
./target/debug/rinflow-agent` by hand.
