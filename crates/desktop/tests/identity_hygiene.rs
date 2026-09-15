//! Enforces the "Application identity" rules from CLAUDE.md:
//!   - Process/binary name is `remote-assist`.
//!   - Product name is "Remote Assist", used as-is in every window title.
//!   - Titles never carry status/role text — that belongs in the window
//!     body — so titles are exactly the product name, nothing appended.
//!
//! This reads the actual `tauri.conf.json` the app ships, so it fails the
//! moment someone edits a title to say something like "Remote Assist -
//! Session Active" instead of updating the in-window status text.

const PRODUCT_NAME: &str = "Remote Assist";
const PROCESS_NAME: &str = "remote-assist";

// Substrings that would mean session/role state leaked into a title bar.
const FORBIDDEN_IN_TITLE: &[&str] = &[
    "session", "active", "waiting", "confirm", "ended", "host", "helper", "-", "—", "(",
];

fn config() -> serde_json::Value {
    let raw = include_str!("../tauri.conf.json");
    serde_json::from_str(raw).expect("tauri.conf.json must be valid JSON")
}

#[test]
fn process_name_matches_the_product_naming_rule() {
    assert_eq!(
        env!("CARGO_PKG_NAME"),
        PROCESS_NAME,
        "the compiled binary's name must be the kebab-case product name"
    );
}

#[test]
fn product_name_in_config_matches_the_naming_rule() {
    let cfg = config();
    assert_eq!(cfg["productName"], PRODUCT_NAME);
}

#[test]
fn every_window_title_is_exactly_the_product_name() {
    let cfg = config();
    let windows = cfg["app"]["windows"].as_array().expect("app.windows must be an array");
    assert!(!windows.is_empty(), "expected at least one configured window");

    for window in windows {
        let title = window["title"].as_str().expect("window title must be a string");
        assert_eq!(
            title, PRODUCT_NAME,
            "window '{}' must title itself with only the product name; \
             status/role text belongs in the window body, not the title bar",
            window["label"]
        );
    }
}

#[test]
fn no_window_title_leaks_session_or_role_state() {
    let cfg = config();
    let windows = cfg["app"]["windows"].as_array().expect("app.windows must be an array");
    for window in windows {
        let title = window["title"].as_str().unwrap_or_default().to_lowercase();
        for forbidden in FORBIDDEN_IN_TITLE {
            assert!(
                !title.contains(forbidden),
                "window '{}' title '{}' must not contain '{forbidden}'",
                window["label"],
                window["title"]
            );
        }
    }
}
