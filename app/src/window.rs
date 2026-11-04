//! The window: devices and their arrangement, pairing, settings. Rendered
//! from the agent's snapshot whenever it changes.

use crate::agent::{AgentClient, Status};
use crate::canvas::ArrangementCanvas;
use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use rinflow_core::ModifierMap;
use rinflow_shell::state::{BackendState, BackendView, Snapshot};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/com/rinflow/linux/ui/window.ui")]
    pub struct RinflowWindow {
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub error_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub capture_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub capture_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub capture_retry: TemplateChild<gtk::Button>,
        #[template_child]
        pub emulation_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub emulation_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub emulation_retry: TemplateChild<gtk::Button>,
        #[template_child]
        pub layout_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub my_devices_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub add_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub nearby_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub sharing_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub paired_only_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub clipboard_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub autostart_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub modifier_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub theme_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub language_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub port_spin: TemplateChild<gtk::SpinButton>,
        #[template_child]
        pub name_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub fingerprint_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub version_row: TemplateChild<adw::ActionRow>,

        pub client: AgentClient,
        pub canvas: OnceCell<ArrangementCanvas>,
        pub device_rows: RefCell<Vec<adw::ActionRow>>,
        pub nearby_rows: RefCell<Vec<adw::ActionRow>>,
        pub empty_rows: RefCell<Vec<adw::ActionRow>>,
        pub pairing_dialog: RefCell<Option<(String, adw::AlertDialog)>>,
        /// Set while the snapshot is applied to the controls, so their
        /// change signals do not echo it back to the agent.
        pub applying: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RinflowWindow {
        const NAME: &'static str = "RinflowWindow";
        type Type = super::RinflowWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action("win.connect", Some(&String::static_variant_type()), |window, _, parameter| {
                if let Some(peer) = parameter.and_then(|p| p.get::<String>()) {
                    window.imp().client.connect_peer(&peer);
                }
            });
            klass.install_action("win.disconnect", Some(&String::static_variant_type()), |window, _, parameter| {
                if let Some(peer) = parameter.and_then(|p| p.get::<String>()) {
                    window.imp().client.disconnect_peer(&peer);
                }
            });
            klass.install_action("win.place", Some(&String::static_variant_type()), |window, _, parameter| {
                if let Some(value) = parameter.and_then(|p| p.get::<String>())
                    && let Some((peer, side)) = value.split_once('|')
                {
                    window.imp().client.place_beside(peer, side);
                }
            });
            klass.install_action("win.unplace", Some(&String::static_variant_type()), |window, _, parameter| {
                if let Some(peer) = parameter.and_then(|p| p.get::<String>()) {
                    window.imp().client.remove_placement(&peer);
                }
            });
            klass.install_action("win.forget", Some(&String::static_variant_type()), |window, _, parameter| {
                if let Some(peer) = parameter.and_then(|p| p.get::<String>()) {
                    window.confirm_forget(&peer);
                }
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for RinflowWindow {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }
    }

    impl WidgetImpl for RinflowWindow {}
    impl WindowImpl for RinflowWindow {}
    impl ApplicationWindowImpl for RinflowWindow {}
    impl AdwApplicationWindowImpl for RinflowWindow {}
}

glib::wrapper! {
    pub struct RinflowWindow(ObjectSubclass<imp::RinflowWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

const THEMES: [&str; 3] = ["system", "light", "dark"];
use rinflow_shell::i18n::{LANGUAGES, fill};

impl RinflowWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    pub fn quit_agent(&self) {
        self.imp().client.quit_agent();
    }

    fn setup(&self) {
        let imp = self.imp();
        let client = imp.client.clone();

        let canvas = ArrangementCanvas::default();
        imp.layout_group.add(&canvas);
        canvas.connect_place(glib::clone!(
            #[strong]
            client,
            move |peer, offset| client.set_placement(peer, offset)
        ));
        canvas.connect_preview(glib::clone!(
            #[strong]
            client,
            move |peer, offset| client.preview_placement(peer, offset)
        ));
        client.connect_preview(glib::clone!(
            #[weak]
            canvas,
            move |peer, offset| canvas.set_preview(peer, offset)
        ));
        let _ = imp.canvas.set(canvas);

        imp.modifier_row.set_model(Some(&gtk::StringList::new(&[
            &gettext("Swap ⌘ and Control (⌘C copies here)"),
            &gettext("Keep each key in its place"),
        ])));
        imp.theme_row.set_model(Some(&gtk::StringList::new(&[
            &gettext("Follow the system"),
            &gettext("Light"),
            &gettext("Dark"),
        ])));
        imp.language_row.set_model(Some(&gtk::StringList::new(&[
            &gettext("Follow the system"),
            // Each language by its own name, so it can be found from any.
            "简体中文",
            "日本語",
            "English",
        ])));

        client.connect_snapshot(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |snapshot| window.apply(snapshot)
        ));
        client.connect_error(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |message| window.toast(message)
        ));
        client.connect_status(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |status| window.status_changed(status)
        ));

        imp.error_banner.connect_button_clicked(glib::clone!(
            #[strong]
            client,
            move |_| client.clear_error()
        ));
        imp.capture_retry.connect_clicked(glib::clone!(
            #[strong]
            client,
            move |_| client.retry_input()
        ));
        imp.emulation_retry.connect_clicked(glib::clone!(
            #[strong]
            client,
            move |_| client.retry_input()
        ));
        imp.add_button.connect_clicked(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_| window.add_device_dialog()
        ));

        self.bind_switch(&imp.sharing_row, "sharing");
        self.bind_switch(&imp.paired_only_row, "paired_only");
        self.bind_switch(&imp.clipboard_row, "clipboard");
        self.bind_switch(&imp.autostart_row, "launch_at_login");
        imp.modifier_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |row| {
                if window.imp().applying.get() {
                    return;
                }
                let map = if row.selected() == 0 { ModifierMap::SWAP } else { ModifierMap::KEEP };
                window.imp().client.update_setting("modifier_map", serde_json::to_value(map).unwrap_or_default());
            }
        ));
        imp.theme_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |row| {
                if window.imp().applying.get() {
                    return;
                }
                let theme = THEMES[(row.selected() as usize).min(2)];
                adw::StyleManager::default().set_color_scheme(match theme {
                    "light" => adw::ColorScheme::ForceLight,
                    "dark" => adw::ColorScheme::ForceDark,
                    _ => adw::ColorScheme::Default,
                });
                window.imp().client.update_setting("theme", theme.into());
            }
        ));
        imp.language_row.connect_selected_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |row| {
                if window.imp().applying.get() {
                    return;
                }
                let language = LANGUAGES[(row.selected() as usize).min(LANGUAGES.len() - 1)];
                window.imp().client.update_setting("language", language.into());
            }
        ));
        imp.port_spin.connect_value_changed(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |spin| {
                if window.imp().applying.get() {
                    return;
                }
                window.imp().client.update_setting("port", (spin.value_as_int() as u16).into());
            }
        ));

        client.start();
    }

    fn bind_switch(&self, row: &adw::SwitchRow, key: &'static str) {
        row.connect_active_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |row| {
                if window.imp().applying.get() {
                    return;
                }
                window.imp().client.update_setting(key, row.is_active().into());
            }
        ));
    }

    fn toast(&self, message: &str) {
        self.imp().toast_overlay.add_toast(adw::Toast::new(message));
    }

    fn status_changed(&self, status: &Status) {
        let imp = self.imp();
        match status {
            Status::Connected => {}
            Status::Connecting => {
                imp.error_banner.set_title(&gettext("Starting the Rinflow agent…"));
                imp.error_banner.set_revealed(true);
            }
            Status::Unreachable(detail) => {
                imp.error_banner.set_title(detail);
                imp.error_banner.set_revealed(true);
            }
        }
    }

    /// Renders one snapshot.
    fn apply(&self, snapshot: &Snapshot) {
        let imp = self.imp();
        if snapshot.quitting {
            if let Some(app) = self.application() {
                app.quit();
            }
            return;
        }
        imp.applying.set(true);

        match &snapshot.last_error {
            Some(error) => {
                imp.error_banner.set_title(error);
                imp.error_banner.set_revealed(true);
            }
            None => imp.error_banner.set_revealed(false),
        }

        apply_backend(&imp.capture_row, &imp.capture_icon, &imp.capture_retry, &snapshot.input.capture, true);
        apply_backend(&imp.emulation_row, &imp.emulation_icon, &imp.emulation_retry, &snapshot.input.emulation, false);

        self.rebuild_devices(snapshot);
        self.apply_pairing(snapshot);

        let settings = &snapshot.settings;
        imp.sharing_row.set_active(settings.sharing);
        imp.paired_only_row.set_active(settings.paired_only);
        imp.clipboard_row.set_active(settings.clipboard);
        imp.clipboard_row.set_subtitle(&clipboard_subtitle(snapshot));
        imp.autostart_row.set_active(snapshot.launch_at_login);
        imp.modifier_row
            .set_selected(if settings.modifier_map == ModifierMap::SWAP { 0 } else { 1 });
        imp.theme_row
            .set_selected(THEMES.iter().position(|t| *t == settings.theme).unwrap_or(0) as u32);
        imp.language_row
            .set_selected(LANGUAGES.iter().position(|l| *l == settings.language).unwrap_or(0) as u32);
        imp.port_spin
            .set_value(f64::from(settings.port.unwrap_or(snapshot.control_port)));
        imp.name_row.set_subtitle(&snapshot.device_name);
        imp.fingerprint_row.set_subtitle(
            &snapshot
                .fingerprint
                .as_deref()
                .map(fingerprint_groups)
                .unwrap_or_else(|| gettext("Not started yet")),
        );
        imp.version_row.set_subtitle(&snapshot.agent_version);

        let connected: HashSet<String> = snapshot
            .sessions
            .iter()
            .filter(|s| s.status == rinflow_shell::state::SessionStatus::Connected)
            .map(|s| s.peer_id.clone())
            .collect();
        if let Some(canvas) = imp.canvas.get() {
            let online: HashSet<String> = snapshot.peers.iter().map(|p| p.id.clone()).collect();
            canvas.set_layout(snapshot.layout.clone(), connected, online, snapshot.driving.clone());
        }
        imp.applying.set(false);
    }

    fn rebuild_devices(&self, snapshot: &Snapshot) {
        let imp = self.imp();
        for row in imp.device_rows.borrow_mut().drain(..) {
            imp.my_devices_group.remove(&row);
        }
        for row in imp.nearby_rows.borrow_mut().drain(..) {
            imp.nearby_group.remove(&row);
        }
        let mut device_rows = Vec::new();
        let mut nearby_rows = Vec::new();

        for device in snapshot.layout.devices.iter().filter(|d| d.remembered) {
            let session = snapshot.sessions.iter().find(|s| s.peer_id == device.peer_id);
            let online = snapshot.peers.iter().any(|p| p.id == device.peer_id);
            let paired = snapshot.paired_peers.contains(&device.peer_id);
            let driving = snapshot.driving.as_deref() == Some(device.peer_id.as_str());
            let row = adw::ActionRow::builder().title(&device.name).build();
            row.add_prefix(&gtk::Image::from_icon_name(platform_icon(&device.platform)));
            let mut subtitle = match (session, online) {
                (Some(s), _) if s.status == rinflow_shell::state::SessionStatus::Connected => {
                    let mut text = gettext("Connected");
                    if let Some(ms) = s.latency_ms {
                        text.push_str(" · ");
                        // Translators: the time a message takes to the other device and back.
                        text.push_str(&fill(&gettext("{ms} ms"), &[("ms", &ms)]));
                    }
                    if s.remote_control {
                        text.push_str(" · ");
                        text.push_str(&gettext("controlling this device"));
                    }
                    if driving {
                        text.push_str(" · ");
                        text.push_str(&gettext("you are controlling it"));
                    }
                    text
                }
                (Some(_), _) => gettext("Connecting…"),
                (None, true) => gettext("Online"),
                (None, false) => gettext("Offline"),
            };
            if let Some(side) = device.side {
                subtitle.push_str(" · ");
                subtitle.push_str(&side_name(side));
            } else if device.offset.is_none() {
                subtitle.push_str(" · ");
                subtitle.push_str(&gettext("not placed"));
            }
            if !paired {
                subtitle.push_str(" · ");
                subtitle.push_str(&gettext("not paired"));
            }
            row.set_subtitle(&subtitle);

            let connected = session.is_some_and(|s| s.status == rinflow_shell::state::SessionStatus::Connected);
            let button = gtk::Button::builder()
                .label(if connected { gettext("Disconnect") } else { gettext("Connect") })
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .sensitive(connected || online)
                .build();
            let target = glib::Variant::from(device.peer_id.as_str());
            button.set_action_name(Some(if connected { "win.disconnect" } else { "win.connect" }));
            button.set_action_target_value(Some(&target));
            row.add_suffix(&button);

            let menu = gio::Menu::new();
            let place = gio::Menu::new();
            for (side, label) in [
                ("left", gettext("Place on the left")),
                ("right", gettext("Place on the right")),
                ("top", gettext("Place above")),
                ("bottom", gettext("Place below")),
            ] {
                place.append(
                    Some(&label),
                    Some(&format!("win.place('{}|{}')", device.peer_id, side)),
                );
            }
            menu.append_section(None, &place);
            let more = gio::Menu::new();
            if device.offset.is_some() {
                more.append(Some(&gettext("Remove placement")), Some(&format!("win.unplace('{}')", device.peer_id)));
            }
            more.append(Some(&gettext("Forget this device")), Some(&format!("win.forget('{}')", device.peer_id)));
            menu.append_section(None, &more);
            let menu_button = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .menu_model(&menu)
                .build();
            row.add_suffix(&menu_button);
            imp.my_devices_group.add(&row);
            device_rows.push(row);
        }
        if device_rows.is_empty() {
            let row = adw::ActionRow::builder()
                .title(gettext("No devices yet"))
                .subtitle(gettext("Devices that connect once stay here. Nearby devices are listed below."))
                .build();
            row.add_css_class("dim-label");
            imp.my_devices_group.add(&row);
            device_rows.push(row);
        }

        let remembered: HashSet<&str> = snapshot
            .layout
            .devices
            .iter()
            .filter(|d| d.remembered)
            .map(|d| d.peer_id.as_str())
            .collect();
        for peer in snapshot.peers.iter().filter(|p| !remembered.contains(p.id.as_str())) {
            let row = adw::ActionRow::builder()
                .title(&peer.name)
                .subtitle(format!("{} · {}", platform_name(&peer.platform), peer.address))
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(platform_icon(&peer.platform)));
            let connecting = snapshot.sessions.iter().any(|s| s.peer_id == peer.id);
            let button = gtk::Button::builder()
                .label(if connecting { gettext("Connecting…") } else { gettext("Connect") })
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .sensitive(!connecting)
                .build();
            button.set_action_name(Some("win.connect"));
            button.set_action_target_value(Some(&glib::Variant::from(peer.id.as_str())));
            row.add_suffix(&button);
            imp.nearby_group.add(&row);
            nearby_rows.push(row);
        }
        for address in &snapshot.manual_addresses {
            let text = address["address"].as_str().unwrap_or_default();
            let status = match address["status"].as_str().unwrap_or_default() {
                "found" => gettext("found"),
                "no_response" => gettext("no response"),
                "unresolved" => gettext("address not found"),
                _ => gettext("searching…"),
            };
            let row = adw::ActionRow::builder()
                .title(text)
                .subtitle(format!("{} · {status}", gettext("Added by address")))
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("network-wired-symbolic"));
            let remove = gtk::Button::builder()
                .icon_name("edit-delete-symbolic")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .tooltip_text(gettext("Remove this address"))
                .build();
            let remaining: Vec<String> = snapshot
                .settings
                .manual_addresses
                .iter()
                .filter(|a| a.as_str() != text)
                .cloned()
                .collect();
            remove.connect_clicked(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_| {
                    window
                        .imp()
                        .client
                        .update_setting("manual_addresses", serde_json::to_value(&remaining).unwrap_or_default());
                }
            ));
            row.add_suffix(&remove);
            imp.nearby_group.add(&row);
            nearby_rows.push(row);
        }
        if nearby_rows.is_empty() {
            let row = adw::ActionRow::builder()
                .title(gettext("Nothing nearby"))
                .subtitle(gettext("Open Rinflow on another device on this network, or add it by address."))
                .build();
            row.add_css_class("dim-label");
            imp.nearby_group.add(&row);
            nearby_rows.push(row);
        }
        *imp.device_rows.borrow_mut() = device_rows;
        *imp.nearby_rows.borrow_mut() = nearby_rows;
    }

    fn apply_pairing(&self, snapshot: &Snapshot) {
        let imp = self.imp();
        let current = imp.pairing_dialog.borrow().as_ref().map(|(peer, _)| peer.clone());
        match (&snapshot.pairing, current) {
            (Some(pairing), Some(peer)) if peer == pairing.peer_id => {
                if let Some((_, dialog)) = imp.pairing_dialog.borrow().as_ref()
                    && pairing.accepted
                {
                    dialog.set_body(&gettext("Waiting for the other device to confirm…"));
                    dialog.set_response_enabled("accept", false);
                }
            }
            (Some(pairing), _) => {
                self.close_pairing();
                let dialog = adw::AlertDialog::builder()
                    // Translators: {name} is the other device's name.
                    .heading(fill(&gettext("Pair with {name}"), &[("name", &pairing.name)]))
                    .body(gettext("Check that the other device shows the same six digits, then accept on both."))
                    .close_response("reject")
                    .default_response("accept")
                    .build();
                let code = gtk::Label::builder()
                    .label(format!("{} {}", &pairing.code[..3.min(pairing.code.len())], &pairing.code[3.min(pairing.code.len())..]))
                    .css_classes(["title-1", "monospace"])
                    .selectable(true)
                    .margin_top(12)
                    .margin_bottom(6)
                    .build();
                dialog.set_extra_child(Some(&code));
                dialog.add_response("reject", &gettext("Reject"));
                dialog.set_response_appearance("reject", adw::ResponseAppearance::Destructive);
                dialog.add_response("accept", &gettext("Accept"));
                dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);
                let peer_id = pairing.peer_id.clone();
                dialog.connect_response(
                    None,
                    glib::clone!(
                        #[weak(rename_to = window)]
                        self,
                        move |_, response| {
                            match response {
                                "accept" => window.imp().client.accept_pairing(&peer_id),
                                _ => window.imp().client.reject_pairing(&peer_id),
                            }
                        }
                    ),
                );
                dialog.connect_closed(glib::clone!(
                    #[weak(rename_to = window)]
                    self,
                    move |_| {
                        *window.imp().pairing_dialog.borrow_mut() = None;
                    }
                ));
                dialog.present(Some(self));
                *imp.pairing_dialog.borrow_mut() = Some((pairing.peer_id.clone(), dialog));
            }
            (None, Some(_)) => self.close_pairing(),
            (None, None) => {}
        }
    }

    fn close_pairing(&self) {
        if let Some((_, dialog)) = self.imp().pairing_dialog.borrow_mut().take() {
            dialog.force_close();
        }
    }

    fn confirm_forget(&self, peer_id: &str) {
        let name = self
            .imp()
            .client
            .snapshot()
            .and_then(|s| s.layout.devices.iter().find(|d| d.peer_id == peer_id).map(|d| d.name.clone()))
            .unwrap_or_else(|| peer_id.to_owned());
        let dialog = adw::AlertDialog::builder()
            // Translators: {name} is a device's name.
            .heading(fill(&gettext("Forget {name}?"), &[("name", &name)]))
            .body(gettext("It is disconnected, unplaced and unpaired. It shows up again as a nearby device while it is online."))
            .close_response("cancel")
            .build();
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("forget", &gettext("Forget"));
        dialog.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
        let peer_id = peer_id.to_owned();
        dialog.connect_response(
            Some("forget"),
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| window.imp().client.forget_device(&peer_id)
            ),
        );
        dialog.present(Some(self));
    }

    fn add_device_dialog(&self) {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Add a Device by Address"))
            .body(gettext("For networks that don't carry discovery (guest Wi-Fi, VPNs): the other device's IP address or name. Only one of the two needs the other's address."))
            .close_response("cancel")
            .default_response("add")
            .build();
        let entry = adw::EntryRow::builder().title(gettext("Address")).build();
        let group = adw::PreferencesGroup::new();
        group.add(&entry);
        dialog.set_extra_child(Some(&group));
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("add", &gettext("Add"));
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.connect_response(
            Some("add"),
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                #[weak]
                entry,
                move |_, _| {
                    let address = entry.text().trim().to_owned();
                    if address.is_empty() {
                        return;
                    }
                    let mut addresses = window
                        .imp()
                        .client
                        .snapshot()
                        .map(|s| s.settings.manual_addresses)
                        .unwrap_or_default();
                    if !addresses.contains(&address) {
                        addresses.push(address);
                    }
                    window
                        .imp()
                        .client
                        .update_setting("manual_addresses", serde_json::to_value(addresses).unwrap_or_default());
                }
            ),
        );
        dialog.present(Some(self));
    }
}

fn apply_backend(row: &adw::ActionRow, icon: &gtk::Image, retry: &gtk::Button, view: &BackendView, capture: bool) {
    for class in ["success", "warning", "error"] {
        row.remove_css_class(class);
    }
    let (subtitle, icon_name, class) = match view.state {
        BackendState::Ready => (
            if capture {
                gettext("Ready: move the pointer off the edge toward a placed device.")
            } else {
                gettext("Ready: placed devices can move their pointer here.")
            },
            "emblem-ok-symbolic",
            Some("success"),
        ),
        BackendState::Prompting => (view.detail.clone(), "dialog-question-symbolic", Some("warning")),
        BackendState::Waiting => (
            if view.detail.is_empty() {
                gettext("Connecting to the desktop…")
            } else {
                view.detail.clone()
            },
            "content-loading-symbolic",
            None,
        ),
        BackendState::Denied => (view.detail.clone(), "dialog-warning-symbolic", Some("error")),
        BackendState::Unavailable => (view.detail.clone(), "action-unavailable-symbolic", None),
    };
    row.set_subtitle(&subtitle);
    icon.set_icon_name(Some(icon_name));
    if let Some(class) = class {
        row.add_css_class(class);
    }
    retry.set_visible(matches!(view.state, BackendState::Denied | BackendState::Waiting) && !view.detail.is_empty());
}

/// What the clipboard switch says: what it does, or why it can't yet.
fn clipboard_subtitle(snapshot: &Snapshot) -> String {
    let emulation = snapshot.input.emulation.state;
    if snapshot.input.clipboard || !snapshot.settings.clipboard {
        gettext("Text and images copied here go along when the pointer moves to another device, and come back with it.")
    } else if emulation == BackendState::Ready {
        gettext("This desktop doesn't share its clipboard with Rinflow. It needs the clipboard portal (GNOME 45 or Plasma 6.2 and newer).")
    } else {
        gettext("Available once other devices may control this one: the clipboard comes with that permission.")
    }
}

fn platform_icon(platform: &str) -> &'static str {
    match platform {
        "android" => "phone-symbolic",
        _ => "computer-symbolic",
    }
}

fn platform_name(platform: &str) -> String {
    match platform {
        "macos" => "macOS".to_owned(),
        "windows" => "Windows".to_owned(),
        "linux" => "Linux".to_owned(),
        "android" => "Android".to_owned(),
        _ => gettext("Device"),
    }
}

fn side_name(side: rinflow_core::layout::Side) -> String {
    use rinflow_core::layout::Side;
    match side {
        Side::Left => gettext("on the left"),
        Side::Right => gettext("on the right"),
        Side::Top => gettext("above"),
        Side::Bottom => gettext("below"),
    }
}

/// `ab12…` → `AB12 CD34 …`, in groups of four.
fn fingerprint_groups(hex: &str) -> String {
    hex.to_ascii_uppercase()
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}
