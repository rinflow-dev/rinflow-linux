//! A stand-in user and application on an X server, for the X11 tests
//! (tests/e2e_x11.py). The user: a mouse and a keyboard, played through
//! XTest, which the server treats as devices, barriers and raw events
//! included. The application: a window over the whole screen that has the
//! keyboard focus and writes down every key, button and focus change it
//! is given.
//!
//! Driven over standard input, one JSON object per line, each answered
//! with one line:
//!
//!   {"cmd":"move","dx":40,"dy":0}        the mouse moves (relative)
//!   {"cmd":"warp","x":900,"y":500}       the pointer is put somewhere
//!   {"cmd":"key","code":38,"down":true}  a key, by X key code
//!   {"cmd":"button","button":1,"down":true}
//!   {"cmd":"pointer"}                    → {"x":…,"y":…}
//!   {"cmd":"events"}                     → {"events":[…]} the window got so far
//!   {"cmd":"clear"}                      forgets them
//!   {"cmd":"grab","on":true}             the application holds the pointer, as an open menu does
//!
//! Usage: xdesk [--no-window] (the server is `DISPLAY`). Without a window
//! it is only the user: for a desktop whose application is a real one.

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{self, ConnectionExt as _, EventMask};
use x11rb::protocol::xtest;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

fn main() {
    let (conn, screen) = x11rb::connect(None).expect("X server");
    let (root, width, height) = {
        let screen = &conn.setup().roots[screen];
        (screen.root, screen.width_in_pixels, screen.height_in_pixels)
    };
    xtest::get_version(&conn, 2, 2).expect("XTest").reply().expect("XTest");
    let window = conn.generate_id().expect("window id");
    let application = !std::env::args().any(|argument| argument == "--no-window");
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        width,
        height,
        0,
        xproto::WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &xproto::CreateWindowAux::new()
            .override_redirect(1)
            .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE | EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::FOCUS_CHANGE),
    )
    .expect("window");
    if application {
        conn.map_window(window).expect("map");
        conn.sync().expect("sync");
        conn.set_input_focus(xproto::InputFocus::POINTER_ROOT, window, x11rb::CURRENT_TIME).expect("focus");
        conn.sync().expect("sync");
    }

    let mut events: Vec<Value> = Vec::new();
    // The focus this window was just given is not news.
    record(&conn, &mut events);
    events.clear();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(command) = serde_json::from_str::<Value>(&line) else { continue };
        let number = |key: &str| command[key].as_i64().unwrap_or(0);
        let fake = |kind: u8, detail: u8, window: u32, x: i16, y: i16| {
            xtest::fake_input(&conn, kind, detail, x11rb::CURRENT_TIME, window, x, y, 0).expect("XTest");
        };
        let mut reply = json!({"ok": true});
        match command["cmd"].as_str() {
            Some("move") => fake(xproto::MOTION_NOTIFY_EVENT, 1, x11rb::NONE, number("dx") as i16, number("dy") as i16),
            Some("warp") => fake(xproto::MOTION_NOTIFY_EVENT, 0, root, number("x") as i16, number("y") as i16),
            Some("key") => {
                let kind = if command["down"].as_bool().unwrap_or(true) { xproto::KEY_PRESS_EVENT } else { xproto::KEY_RELEASE_EVENT };
                fake(kind, number("code") as u8, x11rb::NONE, 0, 0);
            }
            Some("button") => {
                let kind = if command["down"].as_bool().unwrap_or(true) { xproto::BUTTON_PRESS_EVENT } else { xproto::BUTTON_RELEASE_EVENT };
                fake(kind, number("button") as u8, x11rb::NONE, 0, 0);
            }
            Some("pointer") => {
                let pointer = conn.query_pointer(root).expect("pointer").reply().expect("pointer");
                reply = json!({"x": pointer.root_x, "y": pointer.root_y});
            }
            Some("events") => {
                conn.sync().expect("sync");
                record(&conn, &mut events);
                reply = json!({"events": events});
            }
            Some("clear") => {
                conn.sync().expect("sync");
                record(&conn, &mut events);
                events.clear();
            }
            Some("grab") => {
                if command["on"].as_bool().unwrap_or(true) {
                    let status = conn
                        .grab_pointer(false, window, EventMask::BUTTON_PRESS, xproto::GrabMode::ASYNC, xproto::GrabMode::ASYNC, x11rb::NONE, x11rb::NONE, x11rb::CURRENT_TIME)
                        .expect("grab")
                        .reply()
                        .expect("grab")
                        .status;
                    reply = json!({"ok": status == xproto::GrabStatus::SUCCESS});
                } else {
                    conn.ungrab_pointer(x11rb::CURRENT_TIME).expect("ungrab");
                }
            }
            _ => reply = json!({"error": "unknown command"}),
        }
        // Done before the answer: the next command may depend on it.
        conn.sync().expect("sync");
        record(&conn, &mut events);
        let _ = writeln!(stdout, "{reply}");
        let _ = stdout.flush();
    }
}

/// Writes down what the window was given since the last look.
fn record(conn: &RustConnection, events: &mut Vec<Value>) {
    while let Some(event) = conn.poll_for_event().expect("X server") {
        match event {
            Event::KeyPress(key) => events.push(json!({"type": "key", "code": key.detail, "down": true})),
            Event::KeyRelease(key) => events.push(json!({"type": "key", "code": key.detail, "down": false})),
            Event::ButtonPress(button) => events.push(json!({"type": "button", "button": button.detail, "down": true, "x": button.root_x, "y": button.root_y})),
            Event::ButtonRelease(button) => events.push(json!({"type": "button", "button": button.detail, "down": false, "x": button.root_x, "y": button.root_y})),
            Event::FocusIn(focus) => events.push(json!({"type": "focus", "in": true, "grab": focus.mode != xproto::NotifyMode::NORMAL})),
            Event::FocusOut(focus) => events.push(json!({"type": "focus", "in": false, "grab": focus.mode != xproto::NotifyMode::NORMAL})),
            _ => {}
        }
    }
}
