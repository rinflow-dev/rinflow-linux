# Changelog

Notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- X11 sessions (Cinnamon, Xfce, MATE and other desktops): controlling other devices, being controlled, and the shared clipboard, with nothing to allow

### Fixed

- The window's own icons were missing on a desktop without `librsvg2-common`; the package now depends on it
- The agent found no desktop when it was started by the window in a session that had not told systemd about itself
- Files pasted from another device were read-only, so changes to them could not be saved
- Cancelling a paste in the window made the file manager report an error; it now stops as when cancelled there
- Copying more than 10,000 files or folders (or folders more than 32 levels deep) did nothing and said nothing; a notification and the window now say to zip them first, as on Windows

### Known issues

- X11 has been tested on Xfce and Cinnamon in a virtual machine, not yet on a physical one, nor with a touchpad, several monitors, or a Mac or Windows PC as the other device.
- On X11 the pointer does not cross while a menu is open or something is being dragged, and scrolling goes by wheel notches.
- Pasted files don't keep their permissions: a script arrives without its execute bit.
- A cancelled or failed paste leaves the part already written, as the file manager does when its own copy is stopped.

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
- Debian packages for x86_64 and arm64

### Known issues

- Tested on GNOME 48 (Debian 13, x86_64) with a Mac. The arm64 package has only been installed and started in emulation, not used on a desktop. Windows as the other device, KDE as the controlling side, several monitors and Steam Deck hardware have not been tested.
- While this device controls another one, its own pointer stays visible at the screen edge: GNOME up to 50 does not hide it during input capture.
- Before GNOME 51 the desktop asks for the permission to control other devices at every sign-in.
- Pasting files copied on another device needs FUSE (`fuse3`, which the package installs). Without it nothing is pasted and nothing says why.
- If the other device stops answering while its files are pasted, the file manager waits up to 15 seconds before the paste fails.
- With several devices connected, one of them disconnecting can clear files another one put on the clipboard.
- There are no automatic updates.
- X11 sessions and wlroots compositors are not supported. There is no Flatpak yet.
