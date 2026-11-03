//! The language of the text the agent produces itself (error messages the
//! window shows). The user's choice (`language` setting: `zh-CN`, `en-US`),
//! or else whatever the shell reported as the system language.
//!
//! Both versions of a string sit side by side where it is used ([`tr`]):
//! there are few strings and two languages.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

const FOLLOW_SYSTEM: u8 = 0;
const CHINESE: u8 = 1;
const ENGLISH: u8 = 2;

static CHOSEN: AtomicU8 = AtomicU8::new(FOLLOW_SYSTEM);
static SYSTEM_CHINESE: AtomicBool = AtomicBool::new(false);

/// The values of the `language` setting.
pub const LANGUAGES: [&str; 3] = ["system", "zh-CN", "en-US"];

/// Applies the `language` setting: `zh-CN`, `en-US`, or anything else
/// (`system`) to follow the system.
pub fn set_language(language: &str) {
    let chosen = match language {
        "zh-CN" => CHINESE,
        "en-US" => ENGLISH,
        _ => FOLLOW_SYSTEM,
    };
    CHOSEN.store(chosen, Ordering::Relaxed);
}

/// What the system language is, as the shell determined it once at
/// start-up (for example from `LANG` / `LC_MESSAGES`).
pub fn set_system_chinese(chinese: bool) {
    SYSTEM_CHINESE.store(chinese, Ordering::Relaxed);
}

/// Whether to show Simplified Chinese.
pub fn chinese() -> bool {
    match CHOSEN.load(Ordering::Relaxed) {
        CHINESE => true,
        ENGLISH => false,
        _ => SYSTEM_CHINESE.load(Ordering::Relaxed),
    }
}

/// `zh` or `en`, whichever [`chinese`] picks.
pub fn tr(zh: &'static str, en: &'static str) -> &'static str {
    if chinese() { zh } else { en }
}

/// Whether a POSIX locale name (`zh_CN.UTF-8`, `zh_SG`, `zh_Hans`) means
/// Simplified Chinese. Traditional Chinese gets English, like every other
/// language, rather than characters its readers do not use.
pub fn locale_is_simplified_chinese(locale: &str) -> bool {
    let lower = locale.to_ascii_lowercase();
    let tag = lower.split(['.', '@']).next().unwrap_or("");
    matches!(tag, "zh_cn" | "zh_sg" | "zh_hans" | "zh")
        || tag.starts_with("zh_hans")
        || tag == "zh_cn"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_wins_over_the_system_language() {
        set_system_chinese(true);
        set_language("en-US");
        assert_eq!(tr("退出", "Quit"), "Quit");
        set_language("zh-CN");
        assert_eq!(tr("退出", "Quit"), "退出");
        set_language("system");
        assert_eq!(tr("退出", "Quit"), "退出");
        set_system_chinese(false);
        assert_eq!(tr("退出", "Quit"), "Quit");
    }

    #[test]
    fn locales() {
        assert!(locale_is_simplified_chinese("zh_CN.UTF-8"));
        assert!(locale_is_simplified_chinese("zh_SG"));
        assert!(!locale_is_simplified_chinese("zh_TW.UTF-8"));
        assert!(!locale_is_simplified_chinese("en_US.UTF-8"));
        assert!(!locale_is_simplified_chinese("C"));
    }
}
