//! Rinflow's window on Linux: a client of the agent's socket that
//! renders its snapshot with GTK 4 and libadwaita, and sends requests back.

mod agent;
mod canvas;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};
use std::path::PathBuf;

pub const APP_ID: &str = "com.rinflow.linux";
const TEXT_DOMAIN: &str = "rinflow";

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // The language and theme the user chose live in the agent's settings
    // file; read them before GTK and gettext start, so the window opens in
    // them. (The agent applies every other setting.)
    let settings = rinflow_shell::settings::SettingsStore::load(&data_directory()).get();
    let language = match settings.language.as_str() {
        "zh-CN" => Some("zh_CN.UTF-8"),
        "en-US" => Some("en_US.UTF-8"),
        _ => None,
    };
    if let Some(language) = language {
        // SAFETY: before any other thread exists.
        unsafe {
            std::env::set_var("LANGUAGE", language);
            std::env::set_var("LC_MESSAGES", language);
        }
    }
    // SAFETY: before any other thread exists.
    unsafe {
        gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    }
    let _ = gettextrs::bindtextdomain(TEXT_DOMAIN, locale_directory());
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

/// Where the translations are: next to the binary's prefix, or wherever
/// `RINFLOW_LOCALEDIR` says during development.
fn locale_directory() -> PathBuf {
    if let Some(dir) = std::env::var_os("RINFLOW_LOCALEDIR") {
        return PathBuf::from(dir);
    }
    let prefix = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().and_then(|bin| bin.parent()).map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("/usr"));
    prefix.join("share").join("locale")
}

use std::path::Path;
