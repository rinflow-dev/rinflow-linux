//! Rinflow's window on Linux: a client of the agent's socket that
//! renders its snapshot with GTK 4 and libadwaita, and sends requests back.

mod agent;
mod canvas;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};
use std::path::PathBuf;

pub const APP_ID: &str = "com.rinflow.rinflow";
use rinflow_shell::i18n::{self, tr};

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // The language and theme the user chose live in the agent's settings
    // file; read them before GTK starts, so the window opens in them. (The
    // agent applies every other setting.)
    let settings = rinflow_shell::settings::SettingsStore::load(&data_directory()).get();
    // The user's own LANGUAGE, before a chosen language replaces it below.
    let language_list = std::env::var("LANGUAGE").ok().filter(|list| !list.is_empty());
    agent::keep_language_list(language_list.clone());
    // SAFETY: before any other thread exists; setlocale's answer is copied
    // before the next call.
    let messages_locale = unsafe {
        libc::setlocale(libc::LC_ALL, c"".as_ptr());
        let locale = libc::setlocale(libc::LC_MESSAGES, std::ptr::null());
        (!locale.is_null()).then(|| std::ffi::CStr::from_ptr(locale).to_string_lossy().into_owned())
    };
    // Rinflow's own text: from the catalogs, read in-process, so the
    // language can change while the window runs (shell/src/i18n.rs). The
    // window's template is built untranslated (no gettext domain is bound)
    // and translated by the window itself.
    i18n::init(
        i18n::locale_directory(),
        i18n::system_languages(messages_locale.as_deref(), language_list.as_deref()),
    );
    i18n::set_language(&settings.language);
    gtk_strings_language(&settings.language, messages_locale.as_deref());
    // GTK would call setlocale(LC_ALL, "") again, undoing the above.
    gtk::disable_setlocale();

    gio::resources_register_include!("rinflow.gresource").expect("the window's resources");

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .resource_base_path("/com/rinflow/rinflow")
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
            gtk::IconTheme::for_display(&display).add_resource_path("/com/rinflow/rinflow/icons");
            let css = gtk::CssProvider::new();
            css.load_from_resource("/com/rinflow/rinflow/style.css");
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

/// GTK's and libadwaita's own strings (the About dialog's, an entry's menu)
/// come from libc's gettext, which takes the language from the environment:
/// a chosen language goes in `LANGUAGE` here, before any thread exists, so
/// they follow it from the next start. It only picks the translations, so
/// the locale's formats and a locale that isn't installed don't matter;
/// gettext ignores it in the C locale, so then the messages' locale becomes
/// C.UTF-8.
fn gtk_strings_language(setting: &str, messages_locale: Option<&str>) {
    let Some(language) = i18n::gettext_language(setting) else {
        return;
    };
    // SAFETY: before any other thread exists.
    unsafe { std::env::set_var("LANGUAGE", language) };
    if matches!(messages_locale, None | Some("C" | "POSIX")) {
        // SAFETY: as above.
        let set = unsafe { libc::setlocale(libc::LC_MESSAGES, c"C.UTF-8".as_ptr()) };
        if set.is_null() {
            log::warn!("there is no C.UTF-8 locale, so GTK's own strings stay in English");
        }
    }
}

fn setup_actions(app: &adw::Application) {
    let about = gio::ActionEntry::builder("about")
        .activate(|app: &adw::Application, _, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Rinflow")
                .application_icon(APP_ID)
                .developer_name("Rinflow")
                .version(env!("CARGO_PKG_VERSION"))
                .license_type(gtk::License::Mpl20)
                .website("https://github.com/rinflow-dev")
                .comments(tr("Use one mouse and keyboard across your computers."))
                .build();
            // Translators: your names, one per line, shown in the About
            // dialog. Leave it as "translator-credits" to show none.
            let credits = tr("translator-credits");
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

