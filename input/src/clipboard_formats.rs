//! What the wire's clipboard formats are called on a Linux desktop, and
//! which of a desktop's formats Rinflow shares. The same on Wayland (the
//! Clipboard portal, `portal_clipboard.rs`) and on X11 (the CLIPBOARD
//! selection, `x11_clipboard.rs`): both name formats by MIME type, X11
//! programs with a few older names beside them.
//!
//! The wire carries three MIME types (`rinflow_core::MIME_*`), and a
//! fourth for copied files, `text/uri-list`.

use rinflow_core::{Clipboard, ClipboardFormat, MAX_CLIPBOARD_SIZE, MIME_HTML, MIME_PNG, MIME_TEXT};
use std::borrow::Cow;
use std::time::Duration;

/// How long one application may take to hand over or take one format.
pub(crate) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);
/// How long after giving the selection up the desktop's "no owner" is taken
/// for the answer to that.
pub(crate) const GIVE_UP_ECHO: Duration = Duration::from_secs(2);

/// What applications call UTF-8 text, best first. GTK and Qt offer all of
/// the first four; older X11 programs the last two.
pub(crate) const TEXT_TYPES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];
pub(crate) const HTML_TYPE: &str = "text/html";
pub(crate) const PNG_TYPE: &str = "image/png";
/// Password managers (KeePassXC, KWallet, …) mark what they copy with this
/// type, holding `secret`; such contents never leave this device.
pub(crate) const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";
/// Copied files: `file://` URLs, one per line. Also the wire's name for it.
pub(crate) const URI_LIST: &str = "text/uri-list";
/// What Nautilus and other GTK file managers paste from: `copy` or `cut`,
/// then the URLs, one per line.
pub(crate) const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";
/// A list of copied files is small; anything larger is not one.
pub(crate) const MAX_URI_LIST: usize = 1024 * 1024;

/// Reads the formats Rinflow shares out of the desktop's selection, most
/// specific first, within the core's size limit. Empty when a password
/// manager marked it, or nothing shareable is there. `types` is what the
/// selection offers; `read` hands over one of them, at most so many bytes
/// (`None` when it is larger, or the application does not answer).
pub(crate) async fn read_shared(types: &[String], files: bool, mut read: impl AsyncFnMut(&str, usize) -> Option<Vec<u8>>) -> Clipboard {
    let has = |name: &str| types.iter().any(|t| t.eq_ignore_ascii_case(name));
    if has(PASSWORD_HINT) {
        let hint = read(PASSWORD_HINT, 64).await.unwrap_or_default();
        if String::from_utf8_lossy(&hint).trim() == "secret" {
            log::info!("clipboard: a password manager's secret; not shared");
            return Clipboard::default();
        }
    }
    let mut formats = Vec::new();
    let mut budget = MAX_CLIPBOARD_SIZE.saturating_sub(64);
    let mut take = |mime: &str, data: Vec<u8>, budget: &mut usize| {
        let cost = data.len() + mime.len() + 6;
        if cost <= *budget {
            *budget -= cost;
            formats.push(ClipboardFormat { mime: mime.to_owned(), data });
        } else {
            log::info!("clipboard: {mime} ({} bytes) left out, over the size limit", data.len());
        }
    };
    // Copied files come first: the core looks for them there, and sends
    // the rest (the file names as text, say) along.
    if files {
        let list = if has(URI_LIST) {
            read(URI_LIST, MAX_URI_LIST).await.and_then(|bytes| decode_text(&bytes))
        } else if has(GNOME_COPIED_FILES) {
            read(GNOME_COPIED_FILES, MAX_URI_LIST).await.and_then(|bytes| decode_text(&bytes))
        } else {
            None
        };
        if let Some(list) = list.as_deref().map(file_urls).filter(|list| !list.is_empty()) {
            take(URI_LIST, list.into_bytes(), &mut budget);
        }
    }
    if has(HTML_TYPE)
        && let Some(html) = read(HTML_TYPE, budget).await
        && let Some(text) = decode_text(&html)
        && !text.is_empty()
    {
        take(MIME_HTML, text.into_bytes(), &mut budget);
    }
    if has(PNG_TYPE)
        && let Some(png) = read(PNG_TYPE, budget).await
        && png.starts_with(b"\x89PNG\r\n\x1a\n")
    {
        take(MIME_PNG, png, &mut budget);
    }
    if let Some(name) = TEXT_TYPES.iter().find(|name| has(name))
        && let Some(bytes) = read(name, budget).await
        && let Some(text) = decode_text(&bytes)
        && !text.is_empty()
    {
        take(MIME_TEXT, text.into_bytes(), &mut budget);
    }
    // Most specific first: HTML, then an image, then plain text. A copied
    // image in a browser comes as both HTML (<img>) and PNG.
    Clipboard { formats }
}

/// The MIME types to announce for a peer's clipboard, in its order.
pub(crate) fn local_types(clipboard: &Clipboard) -> Vec<String> {
    let mut types = Vec::new();
    for format in &clipboard.formats {
        let names: &[&str] = match format.mime.as_str() {
            URI_LIST => &[URI_LIST, GNOME_COPIED_FILES],
            MIME_HTML => &[HTML_TYPE],
            MIME_PNG => &[PNG_TYPE],
            MIME_TEXT => &TEXT_TYPES,
            _ => &[],
        };
        for name in names {
            if !types.iter().any(|t| t == name) {
                types.push((*name).to_owned());
            }
        }
    }
    types
}

/// The bytes to paste for a local MIME type, if the peer's clipboard has them.
pub(crate) fn data_for<'a>(clipboard: &'a Clipboard, mime: &str) -> Option<Cow<'a, [u8]>> {
    let files = || clipboard.formats.iter().find(|format| format.mime == URI_LIST);
    if mime.eq_ignore_ascii_case(URI_LIST) {
        return files().map(|format| Cow::Borrowed(format.data.as_slice()));
    }
    if mime.eq_ignore_ascii_case(GNOME_COPIED_FILES) {
        let list = file_urls(&String::from_utf8_lossy(&files()?.data));
        return Some(Cow::Owned(format!("copy\n{}", list.replace("\r\n", "\n").trim_end()).into_bytes()));
    }
    let wire = if mime.eq_ignore_ascii_case(HTML_TYPE) || mime.to_ascii_lowercase().starts_with("text/html;") {
        MIME_HTML
    } else if mime.eq_ignore_ascii_case(PNG_TYPE) {
        MIME_PNG
    } else if TEXT_TYPES.iter().any(|t| t.eq_ignore_ascii_case(mime)) {
        MIME_TEXT
    } else {
        return None;
    };
    clipboard
        .formats
        .iter()
        .find(|format| format.mime == wire)
        .map(|format| Cow::Borrowed(format.data.as_slice()))
}

/// The `file://` URLs of a `text/uri-list` (or of Nautilus's form of it,
/// whose first line is `copy` or `cut`), one per line ending in CR LF as
/// the type has it. Comments and anything that is not a local file are
/// left out.
pub(crate) fn file_urls(list: &str) -> String {
    list.lines()
        .map(str::trim)
        .filter(|line| line.starts_with("file://"))
        .map(|line| format!("{line}\r\n"))
        .collect()
}

/// Text as applications hand it over: UTF-8 nearly always; UTF-16 with a
/// byte-order mark from some browsers' HTML. NULs at the end are dropped.
pub(crate) fn decode_text(bytes: &[u8]) -> Option<String> {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16(&units).ok()?
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        String::from_utf16(&units).ok()?
    } else {
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        String::from_utf8_lossy(bytes).into_owned()
    };
    Some(text.trim_end_matches('\0').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clipboard(formats: &[(&str, &[u8])]) -> Clipboard {
        Clipboard {
            formats: formats
                .iter()
                .map(|(mime, data)| ClipboardFormat { mime: (*mime).to_owned(), data: data.to_vec() })
                .collect(),
        }
    }

    #[test]
    fn wire_types_map_to_what_applications_ask_for() {
        let c = clipboard(&[(MIME_HTML, b"<b>hi</b>"), (MIME_TEXT, b"hi"), ("text/rtf", b"{}")]);
        assert_eq!(
            local_types(&c),
            ["text/html", "text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"]
        );
        assert_eq!(data_for(&c, "UTF8_STRING").as_deref(), Some(&b"hi"[..]));
        assert_eq!(data_for(&c, "text/plain;charset=UTF-8").as_deref(), Some(&b"hi"[..]));
        assert_eq!(data_for(&c, "text/html").as_deref(), Some(&b"<b>hi</b>"[..]));
        assert_eq!(data_for(&c, "image/png"), None);
        assert_eq!(data_for(&c, "text/rtf"), None);
        assert!(local_types(&Clipboard::default()).is_empty());
    }

    #[test]
    fn copied_files_are_offered_as_file_managers_take_them() {
        let c = clipboard(&[(URI_LIST, b"file:///home/me/a%20b.txt\r\nfile:///home/me/c\r\n"), (MIME_TEXT, b"a b.txt")]);
        assert_eq!(local_types(&c)[..2], ["text/uri-list", "x-special/gnome-copied-files"]);
        assert_eq!(data_for(&c, "text/uri-list").as_deref(), Some(&b"file:///home/me/a%20b.txt\r\nfile:///home/me/c\r\n"[..]));
        assert_eq!(
            data_for(&c, "x-special/gnome-copied-files").as_deref(),
            Some(&b"copy\nfile:///home/me/a%20b.txt\nfile:///home/me/c"[..])
        );
        // Nautilus's own form, a comment and a remote URL: only local files.
        assert_eq!(file_urls("copy\nfile:///a\n# note\nsftp://host/b\nfile:///c"), "file:///a\r\nfile:///c\r\n");
        assert_eq!(file_urls("just text"), "");
    }

    #[test]
    fn text_decodes_from_what_applications_send() {
        assert_eq!(decode_text("héllo 你好".as_bytes()).unwrap(), "héllo 你好");
        assert_eq!(decode_text(b"\xEF\xBB\xBFbom").unwrap(), "bom");
        assert_eq!(decode_text(b"\xFF\xFEh\0i\0").unwrap(), "hi");
        assert_eq!(decode_text(b"nul\0").unwrap(), "nul");
    }
}
