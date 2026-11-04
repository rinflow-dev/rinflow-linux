//! Rinflow's translations, from GNU gettext catalogs: `po/<language>.po`,
//! compiled to `<locale directory>/<language>/LC_MESSAGES/rinflow.mo`.
//!
//! The window is the only place text is translated. The agent sends its
//! messages untranslated, as [`Text`] (a message id and its values), and the
//! window translates them when it shows them, so everything on screen
//! follows one language and changes at once when the setting does.
//!
//! The source strings are English and are the message ids. Mark them with
//! [`msg`] where a message is sent untranslated (the agent), or with [`tr`],
//! or [`trf`] when they have `{name}` placeholders, where they are shown
//! (never build a sentence from pieces: word order differs between
//! languages). `make pot` extracts them.
//!
//! The catalogs are read here rather than through libc's `gettext`, because
//! the language can change while the window runs and libc takes it from the
//! environment, which a process with threads cannot change safely. Which
//! catalog is used follows libc's rules: the setting if there is one, or
//! else `LANGUAGE` (unless the messages' locale is `C`), then that locale;
//! for each, `ll_CC@modifier`, `ll@modifier`, `ll_CC`, `ll`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

/// The `language` setting last applied with [`set_language`].
pub fn language() -> String {
    STATE.read().unwrap_or_else(|e| e.into_inner()).setting.clone()
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

/// The language list gettext would use, most wanted first: `LANGUAGE`
/// (`language_list`), then the locale libc chose for messages
/// (`messages_locale`, what `setlocale(LC_MESSAGES, NULL)` answers after
/// `setlocale(LC_ALL, "")`). Taking libc's answer rather than `LANG` and
/// `LC_*` means a locale that isn't installed counts as `C`, as it does for
/// GTK's own strings. In the `C` locale `LANGUAGE` is ignored, as libc does.
/// (`LANGUAGE=ja:en` with `ja_JP.UTF-8` → `ja`, `en`, `ja_JP.UTF-8`.)
pub fn system_languages(messages_locale: Option<&str>, language_list: Option<&str>) -> Vec<String> {
    let mut list = Vec::new();
    if let Some(locale) = messages_locale.filter(|l| !l.is_empty() && *l != "C" && *l != "POSIX") {
        if let Some(languages) = language_list {
            list.extend(languages.split(':').filter(|l| !l.is_empty()).map(str::to_owned));
        }
        list.push(locale.to_owned());
    }
    list
}

/// The catalog names a language may be found under, as libc looks for them:
/// `ja_JP.UTF-8` → `ja_JP`, `ja`; `sr_RS@latin` → `sr_RS@latin`,
/// `sr@latin`, `sr_RS`, `sr`. Simplified Chinese has one catalog, `zh_CN`,
/// which `zh`, `zh_SG` and `zh_Hans…` also use (Traditional Chinese, `zh_TW`
/// and `zh_HK`, has none, and gets English rather than Simplified).
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
    if short == "zh" && (base == "zh" || base == "zh_SG" || base.starts_with("zh_Hans")) {
        names.push("zh_CN".to_owned());
    }
    names.push(short.to_owned());
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| !name.is_empty() && name != "C" && name != "POSIX" && seen.insert(name.clone()));
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

/// [`tr`], then each `{name}` replaced with its value.
pub fn trf(msgid: &str, values: &[(&str, &dyn Display)]) -> String {
    fill(&tr(msgid), values)
}

/// Replaces each `{name}` in a translated message with its value, in one
/// pass: a value is never searched for placeholders itself (a device can
/// call itself `{message}`). Unknown placeholders are left as they are.
pub fn fill(message: &str, values: &[(&str, &dyn Display)]) -> String {
    let mut text = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after
            .find('}')
            .and_then(|close| values.iter().find(|(name, _)| *name == &after[..close]).map(|(_, value)| (close, value)));
        match value {
            Some((close, value)) => {
                text.push_str(&value.to_string());
                rest = &after[close + 1..];
            }
            None => {
                text.push('{');
                rest = after;
            }
        }
    }
    text.push_str(rest);
    text
}

/// Text the agent sends the window: one of Rinflow's messages, to be
/// translated where it is shown, or text that is shown as it is.
///
/// On the wire a plain text is a JSON string, a message an object:
/// `{"id": "{name}: {message}", "args": {"name": "Mac", "message": {"id": "Unpaired."}}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Text {
    /// Shown as it is: a name, or an error from elsewhere (a portal, the
    /// core, the system), which has no translation.
    Plain(String),
    /// A message id (the English source) and the values of its `{name}`
    /// placeholders, translated in their turn.
    Message {
        id: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        args: BTreeMap<String, Text>,
    },
}

/// One of Rinflow's messages, sent untranslated (see [`Text`]); add the
/// values of its placeholders with [`Text::arg`].
pub fn msg(id: &str) -> Text {
    Text::Message { id: id.to_owned(), args: BTreeMap::new() }
}

impl Text {
    /// The value of the placeholder `{name}`.
    pub fn arg(mut self, name: &str, value: impl Into<Text>) -> Self {
        if let Self::Message { args, .. } = &mut self {
            args.insert(name.to_owned(), value.into());
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Plain(text) => text.is_empty(),
            Self::Message { id, .. } => id.is_empty(),
        }
    }

    /// In the language [`set_language`] chose (English where nothing
    /// loaded the catalogs, as in the agent).
    pub fn translate(&self) -> String {
        match self {
            Self::Plain(text) => text.clone(),
            Self::Message { id, args } => {
                let args: Vec<(&str, String)> = args.iter().map(|(name, value)| (name.as_str(), value.translate())).collect();
                let values: Vec<(&str, &dyn Display)> = args.iter().map(|(name, value)| (*name, value as &dyn Display)).collect();
                fill(&tr(id), &values)
            }
        }
    }
}

impl Default for Text {
    fn default() -> Self {
        Self::Plain(String::new())
    }
}

impl Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.translate())
    }
}

impl From<String> for Text {
    fn from(text: String) -> Self {
        Self::Plain(text)
    }
}

impl From<&str> for Text {
    fn from(text: &str) -> Self {
        Self::Plain(text.to_owned())
    }
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
        let mut tables = [Vec::new(), Vec::new()];
        let mut strings = Vec::new();
        for (table, texts) in tables.iter_mut().zip([
            entries.iter().map(|e| e.0).collect::<Vec<_>>(),
            entries.iter().map(|e| e.1).collect(),
        ]) {
            for text in texts {
                table.extend((text.len() as u32).to_le_bytes());
                table.extend((data as u32).to_le_bytes());
                strings.extend(text.as_bytes());
                strings.push(0);
                data += text.len() + 1;
            }
        }
        let [originals, translations] = tables;
        [head, originals, translations, strings].concat()
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
        assert_eq!(system_languages(Some("ja_JP.UTF-8"), None), ["ja_JP.UTF-8"]);
        assert_eq!(system_languages(Some("en_US.UTF-8"), Some("zh_CN:en")), ["zh_CN", "en", "en_US.UTF-8"]);
        assert!(system_languages(Some("C"), Some("ja")).is_empty(), "LANGUAGE doesn't apply in the C locale");
        assert!(system_languages(None, Some("ja")).is_empty());
        assert_eq!(system_languages(Some("C.UTF-8"), Some("ja")), ["ja", "C.UTF-8"]);
    }

    #[test]
    fn catalog_names() {
        assert_eq!(candidates("ja_JP.UTF-8"), ["ja_JP", "ja"]);
        assert_eq!(candidates("zh_CN"), ["zh_CN", "zh"]);
        assert_eq!(candidates("zh_SG.UTF-8"), ["zh_SG", "zh_CN", "zh"]);
        assert_eq!(candidates("zh"), ["zh", "zh_CN"]);
        assert_eq!(candidates("zh_Hans_CN"), ["zh_Hans_CN", "zh_CN", "zh"]);
        assert_eq!(candidates("zh_TW.UTF-8"), ["zh_TW", "zh"]);
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
        assert_eq!(language(), "zh-CN");
        assert_eq!(msg("Quit").translate(), "退出", "a message is translated where it is shown");
        assert_eq!(Text::from("Quit").translate(), "Quit", "plain text never is");
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
        // A value is not searched for placeholders: a device may be called
        // anything.
        assert_eq!(
            fill("{name}: {message}", &[("name", &"evil {message}"), ("message", &"gone")]),
            "evil {message}: gone"
        );
        assert_eq!(fill("{unknown} {", &[("name", &"x")]), "{unknown} {");
    }

    #[test]
    fn text_goes_over_the_wire_untranslated() {
        let text = msg("{name}: {message}").arg("name", "Mac").arg("message", msg("Unpaired."));
        let json = serde_json::to_value(&text).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"id": "{name}: {message}", "args": {"name": "Mac", "message": {"id": "Unpaired."}}})
        );
        assert_eq!(serde_json::from_value::<Text>(json).unwrap(), text);
        assert_eq!(serde_json::from_value::<Text>(serde_json::json!("as is")).unwrap(), Text::from("as is"));
        assert!(Text::default().is_empty());
    }
}
