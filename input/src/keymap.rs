//! USB HID keyboard usages (the wire format) ↔ Linux evdev key codes
//! (`linux/input-event-codes.h`), which is what libei and the compositor
//! speak. Keys keep their position, whatever keyboard layout is active, so
//! input methods see real key strokes.

/// The evdev code for a HID usage, or `None` for keys Linux has no code for.
pub fn evdev_code(usage: u32) -> Option<u32> {
    Some(match usage {
        // Letters a–z, in HID order (alphabetical).
        0x04 => 30, // KEY_A
        0x05 => 48, // KEY_B
        0x06 => 46, // KEY_C
        0x07 => 32, // KEY_D
        0x08 => 18, // KEY_E
        0x09 => 33, // KEY_F
        0x0A => 34, // KEY_G
        0x0B => 35, // KEY_H
        0x0C => 23, // KEY_I
        0x0D => 36, // KEY_J
        0x0E => 37, // KEY_K
        0x0F => 38, // KEY_L
        0x10 => 50, // KEY_M
        0x11 => 49, // KEY_N
        0x12 => 24, // KEY_O
        0x13 => 25, // KEY_P
        0x14 => 16, // KEY_Q
        0x15 => 19, // KEY_R
        0x16 => 31, // KEY_S
        0x17 => 20, // KEY_T
        0x18 => 22, // KEY_U
        0x19 => 47, // KEY_V
        0x1A => 17, // KEY_W
        0x1B => 45, // KEY_X
        0x1C => 21, // KEY_Y
        0x1D => 44, // KEY_Z
        // Digits 1–9, 0.
        0x1E..=0x26 => 2 + (usage - 0x1E),
        0x27 => 11,  // KEY_0
        0x28 => 28,  // KEY_ENTER
        0x29 => 1,   // KEY_ESC
        0x2A => 14,  // KEY_BACKSPACE
        0x2B => 15,  // KEY_TAB
        0x2C => 57,  // KEY_SPACE
        0x2D => 12,  // KEY_MINUS
        0x2E => 13,  // KEY_EQUAL
        0x2F => 26,  // KEY_LEFTBRACE
        0x30 => 27,  // KEY_RIGHTBRACE
        0x31 => 43,  // KEY_BACKSLASH
        0x32 => 43,  // ISO # ~: same key position as \ on ISO boards
        0x33 => 39,  // KEY_SEMICOLON
        0x34 => 40,  // KEY_APOSTROPHE
        0x35 => 41,  // KEY_GRAVE
        0x36 => 51,  // KEY_COMMA
        0x37 => 52,  // KEY_DOT
        0x38 => 53,  // KEY_SLASH
        0x39 => 58,  // KEY_CAPSLOCK
        // F1–F10, F11, F12.
        0x3A..=0x43 => 59 + (usage - 0x3A),
        0x44 => 87,  // KEY_F11
        0x45 => 88,  // KEY_F12
        0x46 => 99,  // KEY_SYSRQ (Print Screen)
        0x47 => 70,  // KEY_SCROLLLOCK
        0x48 => 119, // KEY_PAUSE
        0x49 => 110, // KEY_INSERT
        0x4A => 102, // KEY_HOME
        0x4B => 104, // KEY_PAGEUP
        0x4C => 111, // KEY_DELETE
        0x4D => 107, // KEY_END
        0x4E => 109, // KEY_PAGEDOWN
        0x4F => 106, // KEY_RIGHT
        0x50 => 105, // KEY_LEFT
        0x51 => 108, // KEY_DOWN
        0x52 => 103, // KEY_UP
        0x53 => 69,  // KEY_NUMLOCK
        0x54 => 98,  // KEY_KPSLASH
        0x55 => 55,  // KEY_KPASTERISK
        0x56 => 74,  // KEY_KPMINUS
        0x57 => 78,  // KEY_KPPLUS
        0x58 => 96,  // KEY_KPENTER
        0x59 => 79,  // KEY_KP1
        0x5A => 80,  // KEY_KP2
        0x5B => 81,  // KEY_KP3
        0x5C => 75,  // KEY_KP4
        0x5D => 76,  // KEY_KP5
        0x5E => 77,  // KEY_KP6
        0x5F => 71,  // KEY_KP7
        0x60 => 72,  // KEY_KP8
        0x61 => 73,  // KEY_KP9
        0x62 => 82,  // KEY_KP0
        0x63 => 83,  // KEY_KPDOT
        0x64 => 86,  // KEY_102ND (ISO \ | left of Z)
        0x65 => 127, // KEY_COMPOSE (Menu)
        0x66 => 116, // KEY_POWER
        0x67 => 117, // KEY_KPEQUAL
        // F13–F24.
        0x68..=0x73 => 183 + (usage - 0x68),
        0x74 => 134, // KEY_OPEN
        0x75 => 138, // KEY_HELP
        0x76 => 130, // KEY_PROPS (Menu)
        0x77 => 132, // KEY_FRONT (Select)
        0x78 => 128, // KEY_STOP
        0x79 => 129, // KEY_AGAIN
        0x7A => 131, // KEY_UNDO
        0x7B => 137, // KEY_CUT
        0x7C => 133, // KEY_COPY
        0x7D => 135, // KEY_PASTE
        0x7E => 136, // KEY_FIND
        0x7F => 113, // KEY_MUTE
        0x80 => 115, // KEY_VOLUMEUP
        0x81 => 114, // KEY_VOLUMEDOWN
        0x85 => 121, // KEY_KPCOMMA
        0x87 => 89,  // KEY_RO (International 1: JIS \ _)
        0x88 => 93,  // KEY_KATAKANAHIRAGANA
        0x89 => 124, // KEY_YEN
        0x8A => 92,  // KEY_HENKAN
        0x8B => 94,  // KEY_MUHENKAN
        0x8C => 95,  // KEY_KPJPCOMMA
        0x90 => 122, // KEY_HANGEUL
        0x91 => 123, // KEY_HANJA
        0x92 => 90,  // KEY_KATAKANA
        0x93 => 91,  // KEY_HIRAGANA
        0x94 => 85,  // KEY_ZENKAKUHANKAKU
        0xE0 => 29,  // KEY_LEFTCTRL
        0xE1 => 42,  // KEY_LEFTSHIFT
        0xE2 => 56,  // KEY_LEFTALT
        0xE3 => 125, // KEY_LEFTMETA
        0xE4 => 97,  // KEY_RIGHTCTRL
        0xE5 => 54,  // KEY_RIGHTSHIFT
        0xE6 => 100, // KEY_RIGHTALT
        0xE7 => 126, // KEY_RIGHTMETA
        _ => return None,
    })
}

/// The reverse of [`evdev_code`], for keys captured on this device.
pub fn hid_usage(code: u32) -> Option<u32> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static REVERSE: OnceLock<HashMap<u32, u32>> = OnceLock::new();
    let reverse = REVERSE.get_or_init(|| {
        let mut map = HashMap::new();
        // Ascending, so a shared key position keeps its first usage (0x31
        // over the ISO 0x32).
        for usage in 0..=0xFF {
            if let Some(code) = evdev_code(usage) {
                map.entry(code).or_insert(usage);
            }
        }
        map
    });
    reverse.get(&code).copied()
}

/// Pointer buttons: the core's one-based numbers (1 left, 2 right, 3
/// middle, 4 back, 5 forward) ↔ evdev `BTN_*` codes.
pub fn evdev_button(button: u32) -> Option<u32> {
    Some(match button {
        1 => 0x110, // BTN_LEFT
        2 => 0x111, // BTN_RIGHT
        3 => 0x112, // BTN_MIDDLE
        4 => 0x113, // BTN_SIDE (back)
        5 => 0x114, // BTN_EXTRA (forward)
        6 => 0x115, // BTN_FORWARD
        7 => 0x116, // BTN_BACK
        8 => 0x117, // BTN_TASK
        _ => return None,
    })
}

pub fn button_number(code: u32) -> Option<u32> {
    Some(match code {
        0x110 => 1,
        0x111 => 2,
        0x112 => 3,
        0x113 => 4,
        0x114 => 5,
        0x115 => 6,
        0x116 => 7,
        0x117 => 8,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_keys_map_back_to_the_usage_they_play() {
        for usage in 0..=0xFF {
            let Some(code) = evdev_code(usage) else { continue };
            let expected = if usage == 0x32 { 0x31 } else { usage };
            assert_eq!(hid_usage(code), Some(expected), "usage {usage:#x}");
        }
    }

    #[test]
    fn letters_digits_and_function_keys() {
        assert_eq!(evdev_code(0x04), Some(30)); // a
        assert_eq!(evdev_code(0x1D), Some(44)); // z
        assert_eq!(evdev_code(0x1E), Some(2)); // 1
        assert_eq!(evdev_code(0x26), Some(10)); // 9
        assert_eq!(evdev_code(0x27), Some(11)); // 0
        assert_eq!(evdev_code(0x3A), Some(59)); // F1
        assert_eq!(evdev_code(0x43), Some(68)); // F10
        assert_eq!(evdev_code(0x45), Some(88)); // F12
        assert_eq!(evdev_code(0x68), Some(183)); // F13
        assert_eq!(evdev_code(0x73), Some(194)); // F24
    }

    #[test]
    fn every_key_the_mac_sends_is_mapped() {
        // Usages produced by rinflow-macos/Rinflow/Input/HIDKeyMap.swift.
        let mac = [
            0x04, 0x16, 0x07, 0x09, 0x0B, 0x0A, 0x1D, 0x1B, 0x06, 0x19, 0x05, 0x14, 0x1A, 0x08,
            0x15, 0x1C, 0x17, 0x1E, 0x1F, 0x20, 0x21, 0x23, 0x22, 0x2E, 0x26, 0x24, 0x2D, 0x25,
            0x27, 0x30, 0x12, 0x18, 0x2F, 0x0C, 0x13, 0x28, 0x0F, 0x0D, 0x34, 0x0E, 0x33, 0x31,
            0x36, 0x38, 0x11, 0x10, 0x37, 0x2B, 0x2C, 0x35, 0x2A, 0x29, 0xE7, 0xE3, 0xE1, 0x39,
            0xE2, 0xE0, 0xE5, 0xE6, 0xE4, 0x6C, 0x63, 0x55, 0x57, 0x53, 0x54, 0x58, 0x56, 0x67,
            0x62, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F, 0x60, 0x61, 0x3E, 0x3F, 0x40, 0x3C,
            0x41, 0x42, 0x44, 0x46, 0x45, 0x68, 0x43, 0x69, 0x6A, 0x49, 0x4A, 0x4B, 0x4C, 0x3D,
            0x4D, 0x3B, 0x4E, 0x3A, 0x50, 0x4F, 0x51, 0x52,
        ];
        for usage in mac {
            assert!(evdev_code(usage).is_some(), "usage {usage:#x} is not mapped");
        }
    }

    #[test]
    fn buttons_round_trip() {
        for button in 1..=8 {
            assert_eq!(button_number(evdev_button(button).unwrap()), Some(button));
        }
    }
}
