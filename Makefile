# Day-to-day: `make run` builds and starts the window (which starts the
# agent); `make test` runs the unit tests; `make install` puts a release
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
APP_ID := com.rinflow.linux
LOCALEDIR := $(PREFIX)/share/locale
PO := $(wildcard po/*.po)
MO := $(patsubst po/%.po,target/locale/%/LC_MESSAGES/rinflow.mo,$(PO))

.PHONY: all build release test e2e e2e-crossing e2e-clipboard e2e-systemd e2e-gnome e2e-gnome-clipboard e2e-persist e2e-controlled run run-agent clean install uninstall locale pot check

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

locale: $(MO)

target/locale/%/LC_MESSAGES/rinflow.mo: po/%.po
	@mkdir -p $(dir $@)
	msgfmt --check -o $@ $<

# Runs the window from the build tree with the translations beside it.
run: build locale
	RINFLOW_LOCALEDIR=$(CURDIR)/target/locale RUST_LOG=info ./target/debug/rinflow

run-agent: build
	RUST_LOG=debug ./target/debug/rinflow-agent

# Refreshes po/rinflow.pot from the sources; merge into po/*.po with msgmerge.
pot:
	xgettext --from-code=UTF-8 --keyword=_ --language=C --package-name=rinflow -o po/rinflow.pot data/ui/*.blp
	xgettext --from-code=UTF-8 --keyword=gettext --language=C --join-existing -o po/rinflow.pot app/src/*.rs
	for po in $(PO); do msgmerge --update --backup=none $$po po/rinflow.pot; done

check:
	desktop-file-validate data/$(APP_ID).desktop
	appstreamcli validate --no-net data/$(APP_ID).metainfo.xml

install: release locale
	install -Dm755 target/release/rinflow $(DESTDIR)$(PREFIX)/bin/rinflow
	install -Dm755 target/release/rinflow-agent $(DESTDIR)$(PREFIX)/bin/rinflow-agent
	install -Dm644 data/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).metainfo.xml $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/icons/$(APP_ID).svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/$(APP_ID)-symbolic.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	sed 's|/usr/bin/rinflow-agent|$(PREFIX)/bin/rinflow-agent|' data/rinflow-agent.service > target/rinflow-agent.service
	install -Dm644 target/rinflow-agent.service $(DESTDIR)$(PREFIX)/lib/systemd/user/rinflow-agent.service
	for mo in $(MO); do lang=$$(basename $$(dirname $$(dirname $$mo))); install -Dm644 $$mo $(DESTDIR)$(LOCALEDIR)/$$lang/LC_MESSAGES/rinflow.mo; done
	@echo "Installed. Start the agent for this user with: systemctl --user enable --now rinflow-agent"

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/rinflow $(DESTDIR)$(PREFIX)/bin/rinflow-agent
	rm -f $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	rm -f $(DESTDIR)$(PREFIX)/lib/systemd/user/rinflow-agent.service
	for po in $(PO); do rm -f $(DESTDIR)$(LOCALEDIR)/$$(basename $$po .po)/LC_MESSAGES/rinflow.mo; done

clean:
	$(CARGO) clean
