//! emailOO: user-configurable keyboard shortcuts (#2).
//!
//! The values in `ui/shortcuts.slint` are the defaults. Settings stores
//! overrides as `action = Ctrl+Shift+E` lines, and this module parses them into
//! Slint `Keys` values at startup.

use std::collections::HashMap;

/// Action id, default parts. The ids match the property names in
/// `ui/shortcuts.slint`.
pub(crate) const DEFAULTS: &[(&str, &[&str])] = &[
    ("compose", &["Control", "N"]),
    ("search", &["Control", "K"]),
    ("search-alternate", &["Slash"]),
    ("archive", &["A"]),
    ("spam", &["J"]),
    ("trash", &[if cfg!(target_os = "macos") { "Backspace" } else { "Delete" }]),
    ("previous-message", &["UpArrow"]),
    ("next-message", &["DownArrow"]),
    ("open-message", &["Return"]),
    ("select-message", &["Space"]),
    ("switch-pane", &["F6"]),
    ("find", &["Control", "F"]),
    ("zoom-in", &["Control", "Plus"]),
    ("zoom-in-alternate", &["Control", "Equals"]),
    ("zoom-out", &["Control", "HyphenMinus"]),
    ("zoom-reset", &["Control", "Digit0"]),
    ("caret-browsing", &["F7"]),
    ("reply", &["Control", "R"]),
    ("reply-all", &["Control", "Shift", "R"]),
    ("forward", &["Control", "Shift", "F"]),
    ("mark-read", &["Control", "Shift", "U"]),
    ("mark-unread", &["Control", "U"]),
];

fn default_parts(action: &str) -> Option<&'static [&'static str]> {
    DEFAULTS
        .iter()
        .find(|(id, _)| *id == action)
        .map(|(_, parts)| *parts)
}

pub(crate) fn describe(overrides: &HashMap<String, String>) -> String {
    let mut lines = Vec::new();
    for (action, default) in DEFAULTS {
        let value = overrides
            .get(*action)
            .cloned()
            .unwrap_or_else(|| default.join("+"));
        lines.push(format!("{action} = {value}"));
    }
    lines.join("\n")
}

fn normalize_parts(text: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    for raw in text.split('+') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        let lower = part.to_ascii_lowercase();
        let normalized = match lower.as_str() {
            "ctrl" | "control" | "cmd" | "command" => "Control".to_owned(),
            "alt" | "option" | "opt" => "Alt".to_owned(),
            "shift" => "Shift".to_owned(),
            "meta" | "win" | "super" => "Meta".to_owned(),
            "esc" | "escape" => "Escape".to_owned(),
            "enter" | "return" => "Return".to_owned(),
            "space" => "Space".to_owned(),
            "tab" => "Tab".to_owned(),
            "backspace" => "Backspace".to_owned(),
            "delete" | "del" => "Delete".to_owned(),
            "up" => "UpArrow".to_owned(),
            "down" => "DownArrow".to_owned(),
            "left" => "LeftArrow".to_owned(),
            "right" => "RightArrow".to_owned(),
            "+" => "Plus".to_owned(),
            "-" => "HyphenMinus".to_owned(),
            "=" => "Equals".to_owned(),
            _ => {
                if part.len() == 1 && part.chars().all(|c| c.is_ascii_digit()) {
                    format!("Digit{part}")
                } else if part.len() == 1 && part.chars().all(|c| c.is_ascii_alphabetic()) {
                    part.to_ascii_uppercase()
                } else if let Some(number) = lower.strip_prefix('f')
                    && number.chars().all(|c| c.is_ascii_digit())
                {
                    format!("F{number}")
                } else {
                    part.to_owned()
                }
            }
        };
        parts.push(normalized);
    }
    (!parts.is_empty()).then_some(parts)
}

pub(crate) fn parse(text: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for line in text.lines() {
        let Some((action, keys)) = line.split_once('=') else {
            continue;
        };
        let action = action.trim();
        if action.is_empty() || default_parts(action).is_none() {
            continue;
        }
        let keys = keys.trim();
        if keys.is_empty() {
            continue;
        }
        let Some(parts) = normalize_parts(keys) else {
            continue;
        };
        let joined = parts.join("+");
        if default_parts(action)
            .is_some_and(|default| default.join("+") != joined)
        {
            result.insert(action.to_owned(), joined);
        }
    }
    result
}

fn keys(parts: &[&str]) -> Option<slint::Keys> {
    slint::Keys::from_parts(parts.iter().copied()).ok()
}

pub(crate) fn apply(app: &crate::AppWindow, overrides: &HashMap<String, String>) {
    use slint::ComponentHandle;
    let shortcuts = app.global::<crate::Shortcuts>();
    for (action, default) in DEFAULTS {
        let parts = overrides
            .get(*action)
            .map(|value| value.split('+').collect::<Vec<_>>())
            .unwrap_or_else(|| default.iter().copied().collect::<Vec<_>>());
        let Some(value) = keys(&parts) else { continue };
        match *action {
            "compose" => shortcuts.set_compose(value),
            "search" => shortcuts.set_search(value),
            "search-alternate" => shortcuts.set_search_alternate(value),
            "archive" => shortcuts.set_archive(value),
            "spam" => shortcuts.set_spam(value),
            "trash" => shortcuts.set_trash(value),
            "previous-message" => shortcuts.set_previous_message(value),
            "next-message" => shortcuts.set_next_message(value),
            "open-message" => shortcuts.set_open_message(value),
            "select-message" => shortcuts.set_select_message(value),
            "switch-pane" => shortcuts.set_switch_pane(value),
            "find" => shortcuts.set_find(value),
            "zoom-in" => shortcuts.set_zoom_in(value),
            "zoom-in-alternate" => shortcuts.set_zoom_in_alternate(value),
            "zoom-out" => shortcuts.set_zoom_out(value),
            "zoom-reset" => shortcuts.set_zoom_reset(value),
            "caret-browsing" => shortcuts.set_caret_browsing(value),
            "reply" => shortcuts.set_reply(value),
            "reply-all" => shortcuts.set_reply_all(value),
            "forward" => shortcuts.set_forward(value),
            "mark-read" => shortcuts.set_mark_read(value),
            "mark-unread" => shortcuts.set_mark_unread(value),
            _ => {}
        }
    }
}
