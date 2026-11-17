# Changelog

Notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [0.0.1] - 2026-11-17

First Linux version.

### Added

- Control a nearby Mac, PC or Linux machine by pushing the pointer through a screen edge, and be controlled by one, on GNOME and KDE (Wayland)
- Steam Deck Gaming Mode as a controlled device
- Drag canvas for arranging devices around this screen
- Pairing with a six-digit code, and encrypted connections
- Shared clipboard for text, images and files, on by default; files are transferred when they are pasted
- Tray icon, a notification when a device asks to pair, and `rinflowctl` for the command line
- English, Simplified Chinese and Japanese interface, and light and dark themes
- Debian package for x86_64

### Known issues

- Tested on GNOME 48 (Debian 13) with a Mac. Windows as the other device, KDE as the controlling side, several monitors and Steam Deck hardware have not been tested.
- While this device controls another one, its own pointer stays visible at the screen edge: GNOME up to 50 does not hide it during input capture.
- Before GNOME 51 the desktop asks for the permission to control other devices at every sign-in.
- Pasting files copied on another device needs FUSE (`fuse3`, which the package installs). Without it nothing is pasted and nothing says why.
- If the other device stops answering while its files are pasted, the file manager waits up to 15 seconds before the paste fails.
- With several devices connected, one of them disconnecting can clear files another one put on the clipboard.
- There are no automatic updates.
- X11 sessions and wlroots compositors are not supported. There is no Flatpak yet.
