# Day-to-day: `make` (= `make run`) builds, closes any Rinflow already
# running, and starts the window (which starts the agent); `make test` runs the unit tests; `make install` puts a release
# build under PREFIX (default /usr/local) with the desktop files, the
# translations and the systemd user service.
#
# Needs: rustup, pkg-config, libgtk-4-dev, libadwaita-1-dev (>= 1.5),
# blueprint-compiler, gettext.
#   Ubuntu/Debian: sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev blueprint-compiler gettext
#   Fedora:        sudo dnf install gtk4-devel libadwaita-devel blueprint-compiler gettext
#   Arch:          sudo pacman -S gtk4 libadwaita blueprint-compiler gettext

PREFIX ?= /usr/local
DESTDIR ?=
CARGO ?= cargo
APP_ID := com.rinflow.rinflow
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
LOCALEDIR := $(PREFIX)/share/locale
# Translations (GNU gettext): po/LINGUAS names the languages, po/POTFILES
# the files with strings in them. `make pot` refreshes po/rinflow.pot and
# merges it into each po/<language>.po; `make locale` compiles the catalogs
# and the translated desktop entry and metainfo into target/.
LINGUAS := $(shell grep -v '^\#' po/LINGUAS)
POTFILES := $(shell grep -v '^\#' po/POTFILES)
PO := $(addprefix po/,$(addsuffix .po,$(LINGUAS)))
MO := $(patsubst po/%.po,target/locale/%/LC_MESSAGES/rinflow.mo,$(PO))
DATA := target/data/$(APP_ID).desktop target/data/$(APP_ID).metainfo.xml

.PHONY: all build release test check-po e2e e2e-tray e2e-notify e2e-i18n e2e-window-i18n e2e-canvas e2e-window-pairing screenshots e2e-crossing e2e-x11 e2e-clipboard e2e-systemd e2e-gnome e2e-gnome-clipboard e2e-persist e2e-controlled run run-agent stop clean install uninstall deb dev-desktop undev-desktop locale pot check

# `make` alone runs the window; `make all` only builds.
.DEFAULT_GOAL := run

all: build

build:
	$(CARGO) build

release:
	$(CARGO) build --release

test:
	$(CARGO) test --workspace

# Two agents on this machine, through the real core: discovery, pairing,
# connection, placement sync, disconnect. Needs no display.
e2e: build
	python3 -I tests/e2e_two_agents.py ./target/debug/rinflow-agent

# The whole crossing through two agents and the real core, with a fake
# desktop on each side speaking the real portal and libei protocols
# (tests/fakedesk). Needs dbus-daemon.
e2e-crossing: build
	python3 -I tests/e2e_crossing.py ./target/debug/rinflow-agent ./target/debug/fakedesk

# The clipboard following the pointer both ways (text, HTML, PNG), the
# setting on either side, password managers' secrets kept home; through the
# Clipboard portal of tests/fakedesk. Needs dbus-daemon.
e2e-clipboard: build
	python3 -I tests/e2e_clipboard.py ./target/debug/rinflow-agent ./target/debug/fakedesk

# An agent started before any desktop (the systemd user service at login)
# finds the desktop through the systemd manager's environment, and lets go
# when it is gone. tests/fakedesk --systemd stands in for the manager.
e2e-systemd: build
	python3 -I tests/e2e_systemd.py ./target/debug/rinflow-agent ./target/debug/fakedesk

# The crossing and the clipboard on X11: two agents on two X servers (Xvfb),
# a stand-in user and application on each (tests/fakedesk xdesk), a real
# GTK application for the clipboard, then a desktop at 200 %. Needs Xvfb,
# xrdb (x11-xserver-utils), dbus-daemon, python3-gi and gir1.2-gtk-4.0.
e2e-x11: build
	RINFLOW_TEST_LOG=debug python3 -I tests/e2e_x11.py ./target/debug/rinflow-agent ./target/debug/xdesk

# The agent's text goes out untranslated (the window translates it), whatever
# the agent's environment; the language setting is kept and checked.
e2e-i18n: build
	python3 -I tests/e2e_i18n.py ./target/debug/rinflow-agent

# The window tests run on their own X display (Xvfb). Started from inside
# a Wayland session they would inherit it, and AT-SPI's registry would then
# send the test's pointer events the Wayland way, which never reach Xvfb:
# the session's display variables are taken out first.
XVFB := env -u WAYLAND_DISPLAY -u XAUTHORITY XDG_SESSION_TYPE=x11 xvfb-run -a

# The window changes language while it runs: its text, its lists and the
# agent's messages, read through AT-SPI. Needs xvfb-run, dbus, at-spi2-core,
# python3-gi, gir1.2-atspi-2.0 (and imagemagick for SHOTS=<directory>).
e2e-window-i18n: build locale
	$(XVFB) dbus-run-session -- python3 tests/e2e_window_i18n.py ./target/debug/rinflow $(SHOTS)

# The arrangement canvas with a real pointer (XTest through AT-SPI) against
# a stand-in agent: click, drag from the strip, unplace, Escape, previews one
# at a time. Same needs as e2e-window-i18n.
e2e-canvas: build locale
	$(XVFB) -s "-screen 0 1280x1024x24" dbus-run-session -- python3 tests/e2e_canvas.py ./target/debug/rinflow ./target/debug/rinflow-agent $(SHOTS)

# The README's screenshots (docs/images): the window against a stand-in
# agent, in each language, light and dark.
screenshots: build locale
	$(XVFB) -s "-screen 0 1560x1360x24" dbus-run-session -- python3 tests/screenshots.py ./target/debug/rinflow ./target/debug/rinflow-agent docs/images

# The pairing dialog against a stand-in agent: asked once, an answer is
# final, an accepted pairing waits in a banner that can cancel it.
e2e-window-pairing: build locale
	$(XVFB) -s "-screen 0 1280x1024x24" dbus-run-session -- python3 tests/e2e_window_pairing.py ./target/debug/rinflow ./target/debug/rinflow-agent

# The crossing on two real GNOME desktops: Mutter headless + the GNOME portal
# backend on each side; the permission dialogs are pressed through AT-SPI.
# Needs mutter, xdg-desktop-portal-gnome, dbus, at-spi2-core, python3-gi,
# gir1.2-gtk-4.0 (see build-aux/Dockerfile.ubuntu-24.04).
e2e-gnome: build
	python3 tests/e2e_gnome.py ./target/debug/rinflow-agent ./target/debug/injector

# The clipboard on two real GNOME desktops: GTK copies on one, the pointer
# crosses, GTK pastes on the other (text, HTML, PNG). Needs what e2e-gnome needs.
e2e-gnome-clipboard: build
	python3 tests/e2e_gnome_clipboard.py ./target/debug/rinflow-agent ./target/debug/injector

# The permission dialogs are pressed once; the second start must be silent.
# RINFLOW_TEST_DESKTOP=kde runs these two on KWin + xdg-desktop-portal-kde.
e2e-persist: build
	python3 tests/e2e_persist.py ./target/debug/rinflow-agent

# A real desktop as the controlled device, driven by a fake controller.
e2e-controlled: build
	python3 tests/e2e_controlled.py ./target/debug/rinflow-agent ./target/debug/fakedesk

# The tray icon against a stand-in StatusNotifierWatcher: registered, its
# menu read and pressed over D-Bus. Needs dbus and python3-gi.
e2e-tray: build locale
	dbus-run-session -- python3 tests/e2e_tray.py ./target/debug/rinflow-agent

# Notifications while no window is open, against a stand-in notification
# service: a pairing request announced, opened, declined, withdrawn.
e2e-notify: build locale
	dbus-run-session -- python3 tests/e2e_notify.py ./target/debug/rinflow-agent

locale: $(MO) $(DATA)

target/locale/%/LC_MESSAGES/rinflow.mo: po/%.po
	@mkdir -p $(dir $@)
	msgfmt --check -o $@ $<

target/data/$(APP_ID).desktop: data/$(APP_ID).desktop.in $(PO) po/LINGUAS
	@mkdir -p $(dir $@)
	msgfmt --desktop --template=$< -d po -o $@

target/data/$(APP_ID).metainfo.xml: data/$(APP_ID).metainfo.xml.in $(PO) po/LINGUAS
	@mkdir -p $(dir $@)
	msgfmt --xml --template=$< -d po -o $@

# Runs the window from the build tree; it finds the translations in
# target/locale by itself (so does the agent it starts). Whatever Rinflow
# this user already runs (an older window, an agent left in the background,
# the installed user service) is closed first, once the build is done, so
# the window and its agent both come from this build. An agent that already
# runs this very binary is kept: a new one would make GNOME before 51 ask
# for the capture permission again.
run: build locale
	@$(call stop_all,keep-this-build)
	RUST_LOG=info ./target/debug/rinflow

# The agent alone in this terminal, with its log. `make run` from another
# terminal then adds the window and keeps this agent (same build).
run-agent: build
	@$(call stop_all)
	RUST_LOG=debug ./target/debug/rinflow-agent

# Closes every Rinflow of this user, this build's agent too.
stop:
	@$(call stop_all)

# Stops the systemd user service if it runs (it would restart the agent
# otherwise), then every rinflow and rinflow-agent of this user: SIGTERM,
# then SIGKILL for whatever is still there after 5 s. With an argument,
# keeps an agent whose binary is target/debug/rinflow-agent as it is now
# (same file; a rebuilt agent is a new file, so the old one goes).
STOP_PATTERN := ^(rinflow|rinflow-agent)$$
KEEP_BINARY := target/debug/rinflow-agent
define stop_all
if systemctl --user is-active --quiet rinflow-agent.service 2>/dev/null; then \
	  echo "stopping the rinflow-agent user service"; systemctl --user stop rinflow-agent.service; \
	fi; \
	current=$$(stat -c %d:%i $(KEEP_BINARY) 2>/dev/null); pids=""; \
	for pid in $$(pgrep -u "$$(id -u)" '$(STOP_PATTERN)'); do \
	  if [ -n "$(1)" ] && [ -n "$$current" ] && [ "$$(stat -L -c %d:%i /proc/$$pid/exe 2>/dev/null)" = "$$current" ]; then \
	    echo "keeping the agent ($$pid): already this build"; \
	  else pids="$$pids $$pid"; fi; \
	done; \
	if [ -n "$$pids" ]; then \
	  echo "closing:$$(for p in $$pids; do printf ' %s %s' $$p "$$(cat /proc/$$p/comm 2>/dev/null)"; done)"; \
	  kill $$pids 2>/dev/null; \
	  for i in $$(seq 50); do \
	    alive=""; for p in $$pids; do kill -0 $$p 2>/dev/null && alive="$$alive $$p"; done; \
	    [ -z "$$alive" ] && break; sleep 0.1; \
	  done; \
	  if [ -n "$$alive" ]; then kill -KILL $$alive 2>/dev/null; echo "killed what was still running after 5 s:$$alive"; fi; \
	fi
endef

# Refreshes po/rinflow.pot from the files in po/POTFILES and merges it into
# each language. Rust and Blueprint are read as C (xgettext has no Rust
# before gettext 0.24): mark strings with tr("…") or trf("…", …) where they
# are shown, msg("…") where they are sent untranslated (the agent), _("…")
# in Blueprint; and put a "// Translators:"
# comment right above anything that needs explaining. Entries the sources
# no longer have are dropped (msgattrib --no-obsolete).
pot:
	xgettext --from-code=UTF-8 --add-comments=Translators --package-name=rinflow \
		--package-version=$(VERSION) \
		--language=C --keyword=_ --keyword=C_:1c,2 --keyword=tr --keyword=trf --keyword=msg \
		-o po/rinflow.pot $(filter %.rs %.blp,$(POTFILES))
	xgettext --from-code=UTF-8 --add-comments=Translators --join-existing \
		-o po/rinflow.pot $(filter %.in,$(POTFILES))
	for po in $(PO); do msgmerge --quiet --update --backup=none $$po po/rinflow.pot && msgattrib --no-obsolete -o $$po $$po; done

# Every language complete and well-formed: nothing untranslated or fuzzy,
# the same {placeholders} as the English, catalogs that msgfmt accepts.
check-po:
	python3 -I tests/check_po.py po/rinflow.pot $(PO)

check: locale check-po
	desktop-file-validate target/data/$(APP_ID).desktop
	appstreamcli validate --no-net target/data/$(APP_ID).metainfo.xml

install: release locale
	install -Dm755 target/release/rinflow $(DESTDIR)$(PREFIX)/bin/rinflow
	install -Dm755 target/release/rinflow-agent $(DESTDIR)$(PREFIX)/bin/rinflow-agent
	install -Dm755 target/release/rinflowctl $(DESTDIR)$(PREFIX)/bin/rinflowctl
	install -Dm644 target/data/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 target/data/$(APP_ID).metainfo.xml $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/icons/$(APP_ID).svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/$(APP_ID)-symbolic.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	sed 's|/usr/bin/rinflow-agent|$(PREFIX)/bin/rinflow-agent|' data/rinflow-agent.service > target/rinflow-agent.service
	install -Dm644 target/rinflow-agent.service $(DESTDIR)$(PREFIX)/lib/systemd/user/rinflow-agent.service
	for mo in $(MO); do lang=$$(basename $$(dirname $$(dirname $$mo))); install -Dm644 $$mo $(DESTDIR)$(LOCALEDIR)/$$lang/LC_MESSAGES/rinflow.mo; done
	@echo "Installed. Start the agent for this user with: systemctl --user enable --now rinflow-agent"

# The Debian package (needs cargo-deb: cargo install cargo-deb), in
# target/debian/.
deb: release locale
	cargo deb -p rinflow --no-build

# For running from the build directory: the desktop only shows Rinflow's
# icon and name for a window whose desktop entry it can find. Puts the
# entry (starting this build) and the icons under ~/.local/share.
USER_DATA := $(or $(XDG_DATA_HOME),$(HOME)/.local/share)
dev-desktop: target/data/$(APP_ID).desktop
	mkdir -p $(USER_DATA)/applications
	sed 's|^Exec=rinflow$$|Exec=$(CURDIR)/target/debug/rinflow|' target/data/$(APP_ID).desktop > $(USER_DATA)/applications/$(APP_ID).desktop
	install -Dm644 data/icons/$(APP_ID).svg $(USER_DATA)/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/$(APP_ID)-symbolic.svg $(USER_DATA)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	-update-desktop-database -q $(USER_DATA)/applications

# No icon cache is written there: a cache that outlives its icons hides the
# installed ones (the desktop believes the cache and finds no file).
undev-desktop:
	rm -f $(USER_DATA)/applications/$(APP_ID).desktop
	rm -f $(USER_DATA)/icons/hicolor/scalable/apps/$(APP_ID).svg $(USER_DATA)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	-update-desktop-database -q $(USER_DATA)/applications

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/rinflow $(DESTDIR)$(PREFIX)/bin/rinflow-agent $(DESTDIR)$(PREFIX)/bin/rinflowctl
	rm -f $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	rm -f $(DESTDIR)$(PREFIX)/lib/systemd/user/rinflow-agent.service
	for po in $(PO); do rm -f $(DESTDIR)$(LOCALEDIR)/$$(basename $$po .po)/LC_MESSAGES/rinflow.mo; done

clean:
	$(CARGO) clean
