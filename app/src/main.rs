//! Rinflow's window on Linux: a client of the agent's socket that
//! renders its snapshot with GTK 4 and libadwaita, and sends requests back.

mod agent;
mod canvas;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};
use std::path::PathBuf;

pub const APP_ID: &str = "com.rinflow.linux";
use rinflow_shell::i18n::{self, TEXT_DOMAIN};

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // The language and theme the user chose live in the agent's settings
    // file; read them before GTK and gettext start, so the window opens in
    // them. (The agent applies every other setting.)
    let settings = rinflow_shell::settings::SettingsStore::load(&data_directory()).get();
    // A chosen language goes in LANGUAGE, which only picks the translations
    // (GTK's and libadwaita's too), so the locale's formats and a locale
    // that isn't generated don't matter. gettext ignores LANGUAGE in the C
    // locale, so then the messages' locale becomes C.UTF-8. Following the
    // system is left to libc, as in the agent (shell/src/i18n.rs).
    let chosen = i18n::gettext_language(&settings.language);
    if let Some(language) = chosen {
        // SAFETY: before any other thread exists.
        unsafe { std::env::set_var("LANGUAGE", language) };
    }
    // SAFETY: before any other thread exists; setlocale's answer is read
    // before the next call.
    unsafe {
        gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
        let messages = libc::setlocale(libc::LC_MESSAGES, std::ptr::null());
        let messages = (!messages.is_null()).then(|| std::ffi::CStr::from_ptr(messages).to_bytes());
        if chosen.is_some() && matches!(messages, None | Some(b"C") | Some(b"POSIX")) {
            gettextrs::setlocale(gettextrs::LocaleCategory::LcMessages, "C.UTF-8");
        }
    }
    let _ = gettextrs::bindtextdomain(TEXT_DOMAIN, i18n::locale_directory());
    let _ = gettextrs::bind_textdomain_codeset(TEXT_DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(TEXT_DOMAIN);

    gio::resources_register_include!("rinflow.gresource").expect("the window's resources");

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .resource_base_path("/com/rinflow/linux")
        .build();
    let theme = settings.theme.clone();
    app.connect_startup(move |app| {
        let style = adw::StyleManager::default();
        style.set_color_scheme(match theme.as_str() {
            "light" => adw::ColorScheme::ForceLight,
            "dark" => adw::ColorScheme::ForceDark,
            _ => adw::ColorScheme::Default,
        });
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display).add_resource_path("/com/rinflow/linux/icons");
            let css = gtk::CssProvider::new();
            css.load_from_resource("/com/rinflow/linux/style.css");
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        setup_actions(app);
    });
    app.connect_activate(|app| {
        let window = match app.active_window() {
            Some(window) => window,
            None => window::RinflowWindow::new(app).upcast(),
        };
        window.present();
    });
    app.run()
}

fn setup_actions(app: &adw::Application) {
    let about = gio::ActionEntry::builder("about")
        .activate(|app: &adw::Application, _, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Rinflow")
                .application_icon(APP_ID)
                .developer_name("Rinflow")
                .version(env!("CARGO_PKG_VERSION"))
                .license_type(gtk::License::Gpl30)
                .website("https://github.com/rinflow-dev")
                .comments(gettextrs::gettext(
                    "Use one mouse and keyboard across your computers.",
                ))
                .build();
            // Translators: your names, one per line, shown in the About dialog.
            let credits = gettextrs::gettext("translator-credits");
            if credits != "translator-credits" {
                dialog.set_translator_credits(&credits);
            }
            dialog.present(app.active_window().as_ref());
        })
        .build();
    let quit_agent = gio::ActionEntry::builder("quit-agent")
        .activate(|app: &adw::Application, _, _| {
            if let Some(window) = app
                .active_window()
                .and_downcast::<window::RinflowWindow>()
            {
                window.quit_agent();
            }
        })
        .build();
    app.add_action_entries([about, quit_agent]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
}

/// The agent's data directory (`~/.local/share/rinflow`), as `paths.rs`
/// in the agent computes it.
pub fn data_directory() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            home.push(".local");
            home.push("share");
            home
        });
    base.join("rinflow")
}

