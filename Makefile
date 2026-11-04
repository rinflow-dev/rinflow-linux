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

.PHONY: all build release test check-po e2e e2e-i18n e2e-window-i18n e2e-crossing e2e-clipboard e2e-systemd e2e-gnome e2e-gnome-clipboard e2e-persist e2e-controlled run run-agent clean install uninstall locale pot check

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

# The agent's text goes out untranslated (the window translates it), whatever
# the agent's environment; the language setting is kept and checked.
e2e-i18n: build
	python3 -I tests/e2e_i18n.py ./target/debug/rinflow-agent

# The window changes language while it runs: its text, its lists and the
# agent's messages, read through AT-SPI. Needs xvfb-run, dbus, at-spi2-core,
# python3-gi, gir1.2-atspi-2.0 (and imagemagick for SHOTS=<directory>).
e2e-window-i18n: build locale
	xvfb-run -a dbus-run-session -- python3 tests/e2e_window_i18n.py ./target/debug/rinflow $(SHOTS)

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
# target/locale by itself (so does the agent it starts).
run: build locale
	RUST_LOG=info ./target/debug/rinflow

run-agent: build
	RUST_LOG=debug ./target/debug/rinflow-agent

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
	install -Dm644 target/data/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 target/data/$(APP_ID).metainfo.xml $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
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
