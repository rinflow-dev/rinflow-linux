//! Translations of the text the agent produces itself (the states and
//! errors the window shows), from the same GNU gettext catalogs as the
//! window: `po/<language>.po`, compiled to
//! `<locale directory>/<language>/LC_MESSAGES/rinflow.mo`.
//!
//! The source strings are English and are the message ids: mark them with
//! [`tr`], or [`trf`] when they have `{name}` placeholders (never build a
//! sentence from pieces: word order differs between languages), or
//! [`gettext_noop`] to keep the English and translate it later. `make pot`
//! extracts them.
//!
//! The catalogs are read here rather than through libc's `gettext`, because
//! the language can change while the agent runs (the `language` setting)
//! and libc takes it from the environment, which a process with threads
//! cannot change safely. Which catalog is used follows libc's rules, so the
//! agent and the window always agree: the setting if there is one, or else
//! `LANGUAGE` (unless the locale is `C`), then `LC_ALL`, `LC_MESSAGES`,
//! `LANG`; for each, `ll_CC@modifier`, `ll@modifier`, `ll_CC`, `ll`.

use std::collections::HashMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// The gettext domain of every Rinflow string, the window's and the agent's.
pub const TEXT_DOMAIN: &str = "rinflow";

/// The values of the `language` setting: follow the system, or one of the
/// languages Rinflow is translated into (English is the source).
pub const LANGUAGES: [&str; 4] = ["system", "zh-CN", "ja-JP", "en-US"];

/// What a `language` setting asks gettext for, as a `LANGUAGE` value
/// (`None`: follow the system).
pub fn gettext_language(setting: &str) -> Option<&'static str> {
    match setting {
        "zh-CN" => Some("zh_CN"),
        "ja-JP" => Some("ja_JP:ja"),
        "en-US" => Some("en_US:en"),
        _ => None,
    }
}

#[derive(Default)]
struct Translations {
    directory: Option<PathBuf>,
    system: Vec<String>,
    setting: String,
    catalog: Option<Arc<Catalog>>,
}

static STATE: RwLock<Translations> = RwLock::new(Translations {
    directory: None,
    system: Vec::new(),
    setting: String::new(),
    catalog: None,
});

/// Where the catalogs are and what the system language is (see
/// [`system_languages`]); call once at start-up, before [`set_language`].
pub fn init(directory: PathBuf, system: Vec<String>) {
    let mut state = STATE.write().unwrap_or_else(|e| e.into_inner());
    state.directory = Some(directory);
    state.system = system;
    reload(&mut state);
}

/// Applies the `language` setting: one of [`LANGUAGES`], or anything else
/// to follow the system.
pub fn set_language(setting: &str) {
    let mut state = STATE.write().unwrap_or_else(|e| e.into_inner());
    state.setting = setting.to_owned();
    reload(&mut state);
}

fn reload(state: &mut Translations) {
    let wanted: Vec<String> = match gettext_language(&state.setting) {
        Some(list) => list.split(':').map(str::to_owned).collect(),
        None => state.system.clone(),
    };
    state.catalog = state.directory.as_deref().and_then(|directory| {
        wanted.iter().flat_map(|language| candidates(language)).find_map(|language| {
            let path = directory.join(&language).join("LC_MESSAGES").join(format!("{TEXT_DOMAIN}.mo"));
            let bytes = std::fs::read(&path).ok()?;
            match Catalog::parse(&bytes) {
                Some(catalog) => {
                    log::debug!("translations: {}", path.display());
                    Some(Arc::new(catalog))
                }
                None => {
                    log::warn!("{} isn't a gettext catalog", path.display());
                    None
                }
            }
        })
    });
}

/// The language list gettext would use with this environment, most wanted
/// first (`LANGUAGE=ja:en LANG=de_DE.UTF-8` → `ja`, `en`, `de_DE.UTF-8`).
pub fn system_languages(var: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let var = |name: &str| var(name).filter(|value| !value.is_empty());
    let locale = var("LC_ALL").or_else(|| var("LC_MESSAGES")).or_else(|| var("LANG"));
    let mut list = Vec::new();
    if let Some(locale) = &locale
        && locale != "C"
        && locale != "POSIX"
    {
        if let Some(languages) = var("LANGUAGE") {
            list.extend(languages.split(':').filter(|l| !l.is_empty()).map(str::to_owned));
        }
        list.push(locale.clone());
    }
    list
}

/// The catalog names a language may be found under, as libc looks for them:
/// `ja_JP.UTF-8` → `ja_JP`, `ja`; `sr_RS@latin` → `sr_RS@latin`,
/// `sr@latin`, `sr_RS`, `sr`.
fn candidates(language: &str) -> Vec<String> {
    let (rest, modifier) = match language.split_once('@') {
        Some((rest, modifier)) => (rest, Some(modifier)),
        None => (language, None),
    };
    let base = rest.split('.').next().unwrap_or(rest);
    let short = base.split('_').next().unwrap_or(base);
    let mut names = Vec::new();
    if let Some(modifier) = modifier {
        names.push(format!("{base}@{modifier}"));
        names.push(format!("{short}@{modifier}"));
    }
    names.push(base.to_owned());
    names.push(short.to_owned());
    names.dedup();
    names.retain(|name| !name.is_empty() && name != "C" && name != "POSIX");
    names
}

/// A message in the user's language, or the English source if it has no
/// translation.
pub fn tr(msgid: &str) -> String {
    let state = STATE.read().unwrap_or_else(|e| e.into_inner());
    state
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.messages.get(msgid))
        .cloned()
        .unwrap_or_else(|| msgid.to_owned())
}

/// Marks a message for translation without translating it (for code that
/// keeps the English and translates it later with [`tr`]).
pub const fn gettext_noop(msgid: &'static str) -> &'static str {
    msgid
}

/// [`tr`], then each `{name}` replaced with its value.
pub fn trf(msgid: &str, values: &[(&str, &dyn Display)]) -> String {
    fill(&tr(msgid), values)
}

/// Replaces each `{name}` in a translated message with its value.
pub fn fill(message: &str, values: &[(&str, &dyn Display)]) -> String {
    let mut text = message.to_owned();
    for (name, value) in values {
        text = text.replace(&format!("{{{name}}}"), &value.to_string());
    }
    text
}

/// Where the catalogs are installed: `RINFLOW_LOCALEDIR` if set;
/// `<prefix>/share/locale` for a binary in `<prefix>/bin`; or `target/locale`
/// for one in `target/debug` (what `make locale` builds).
pub fn locale_directory() -> PathBuf {
    if let Some(directory) = std::env::var_os("RINFLOW_LOCALEDIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(directory);
    }
    let exe_directory = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("/usr/bin"));
    let prefix = exe_directory.parent().unwrap_or(Path::new("/usr"));
    let installed = prefix.join("share").join("locale");
    let built = prefix.join("locale");
    if !installed.is_dir() && built.is_dir() { built } else { installed }
}

/// A compiled gettext catalog (`.mo`), singular messages only.
struct Catalog {
    messages: HashMap<String, String>,
}

impl Catalog {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let word = |at: usize, big: bool| -> Option<u32> {
            let raw: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
            Some(if big { u32::from_be_bytes(raw) } else { u32::from_le_bytes(raw) })
        };
        let big = match word(0, false)? {
            0x9504_12de => false,
            0xde12_0495 => true,
            _ => return None,
        };
        if word(4, big)? >> 16 > 1 {
            return None;
        }
        let count = word(8, big)? as usize;
        let originals = word(12, big)? as usize;
        let translations = word(16, big)? as usize;
        let string = |table: usize, index: usize| -> Option<&str> {
            let length = word(table + index * 8, big)? as usize;
            let offset = word(table + index * 8 + 4, big)? as usize;
            std::str::from_utf8(bytes.get(offset..offset.checked_add(length)?)?).ok()
        };
        let mut messages = HashMap::with_capacity(count);
        for index in 0..count {
            let original = string(originals, index)?;
            let translation = string(translations, index)?;
            // The header has the empty id; `context\x04id` has a context;
            // `id\0plural` and `one\0other` are plural forms (the first is
            // the singular).
            let id = original.split('\0').next().unwrap_or("");
            let text = translation.split('\0').next().unwrap_or("");
            if !id.is_empty() && !id.contains('\u{4}') && !text.is_empty() {
                messages.insert(id.to_owned(), text.to_owned());
            }
        }
        Some(Self { messages })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A catalog as msgfmt writes it (little-endian, no hash table).
    fn mo(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut entries: Vec<_> = entries.to_vec();
        entries.sort();
        let n = entries.len();
        let originals = 28;
        let translations = originals + n * 8;
        let mut data = translations + n * 8;
        let mut head = Vec::new();
        for value in [0x9504_12de_u32, 0, n as u32, originals as u32, translations as u32, 0, data as u32] {
            head.extend(value.to_le_bytes());
        }
        let mut tables = (Vec::new(), Vec::new());
        let mut strings = Vec::new();
        for (side, pick) in [(0, 0), (1, 1)] {
            for entry in &entries {
                let text = if pick == 0 { entry.0 } else { entry.1 };
                let table = if side == 0 { &mut tables.0 } else { &mut tables.1 };
                table.extend((text.len() as u32).to_le_bytes());
                table.extend((data as u32).to_le_bytes());
                strings.extend(text.as_bytes());
                strings.push(0);
                data += text.len() + 1;
            }
        }
        [head, tables.0, tables.1, strings].concat()
    }

    #[test]
    fn catalogs_are_read_as_msgfmt_writes_them() {
        let bytes = mo(&[
            ("", "Content-Type: text/plain; charset=UTF-8\n"),
            ("Offline", "オフライン"),
            ("menu\u{4}Open", "開く"),
            ("{n} device\0{n} devices", "{n} 台のデバイス"),
        ]);
        let catalog = Catalog::parse(&bytes).expect("a catalog");
        assert_eq!(catalog.messages.get("Offline").map(String::as_str), Some("オフライン"));
        assert_eq!(catalog.messages.get("{n} device").map(String::as_str), Some("{n} 台のデバイス"));
        assert!(!catalog.messages.contains_key(""), "the header isn't a message");
        assert!(!catalog.messages.contains_key("Open"), "a message with a context isn't the plain one");
        assert!(Catalog::parse(b"not a catalog at all").is_none());
        assert!(Catalog::parse(&bytes[..20]).is_none(), "cut short");
    }

    #[test]
    fn the_system_language_follows_gettexts_rules() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(n, _)| *n == name).map(|(_, v)| v.to_string())
        };
        assert_eq!(system_languages(env(&[("LANG", "ja_JP.UTF-8")])), ["ja_JP.UTF-8"]);
        assert_eq!(
            system_languages(env(&[("LANGUAGE", "zh_CN:en"), ("LANG", "en_US.UTF-8")])),
            ["zh_CN", "en", "en_US.UTF-8"]
        );
        assert_eq!(
            system_languages(env(&[("LC_ALL", "de_DE.UTF-8"), ("LC_MESSAGES", "ja_JP.UTF-8"), ("LANG", "fr_FR")])),
            ["de_DE.UTF-8"]
        );
        assert!(system_languages(env(&[("LANGUAGE", "ja"), ("LANG", "C")])).is_empty(), "LANGUAGE doesn't apply in the C locale");
        assert_eq!(system_languages(env(&[("LANGUAGE", "ja"), ("LANG", "C.UTF-8")])), ["ja", "C.UTF-8"]);
        assert!(system_languages(env(&[])).is_empty());
    }

    #[test]
    fn catalog_names() {
        assert_eq!(candidates("ja_JP.UTF-8"), ["ja_JP", "ja"]);
        assert_eq!(candidates("zh_CN"), ["zh_CN", "zh"]);
        assert_eq!(candidates("ja"), ["ja"]);
        assert_eq!(candidates("sr_RS.UTF-8@latin"), ["sr_RS@latin", "sr@latin", "sr_RS", "sr"]);
        assert!(candidates("C.UTF-8").is_empty());
    }

    #[test]
    fn the_setting_wins_over_the_system_language() {
        let directory = std::env::temp_dir().join(format!("rinflow-i18n-{}", std::process::id()));
        for (language, text) in [("ja", "終了"), ("zh_CN", "退出")] {
            let messages = directory.join(language).join("LC_MESSAGES");
            std::fs::create_dir_all(&messages).unwrap();
            std::fs::write(messages.join("rinflow.mo"), mo(&[("Quit", text)])).unwrap();
        }
        // Traditional Chinese has no catalog: English, not Simplified.
        init(directory.clone(), vec!["zh_TW.UTF-8".into()]);
        set_language("system");
        assert_eq!(tr("Quit"), "Quit");
        set_language("zh-CN");
        assert_eq!(tr("Quit"), "退出");
        set_language("ja-JP");
        assert_eq!(tr("Quit"), "終了");
        assert_eq!(tr("Not translated"), "Not translated");
        set_language("en-US");
        assert_eq!(tr("Quit"), "Quit");
        init(directory.clone(), vec!["ja_JP.UTF-8".into()]);
        set_language("system");
        assert_eq!(tr("Quit"), "終了");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn placeholders() {
        assert_eq!(
            fill("{name} と {name} : {port}", &[("name", &"Mac"), ("port", &42421)]),
            "Mac と Mac : 42421"
        );
    }
}
