//! emailOO: HTML compose editor for the separate compose window (issue #3).
//! A native WKWebView hosting Squire (Fastmail's email editor) is laid over
//! the ComposeWindow's editor area and follows it as the window resizes.
//! Remote content is blocked by the page's CSP and navigation is refused.

use crate::ComposeWindow;
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

const TEMPLATE: &str = include_str!("../resources/editor/editor.html");
const SQUIRE: &str = include_str!("../resources/editor/squire.js");
const PURIFY: &str = include_str!("../resources/editor/purify.min.js");

pub(crate) struct EditorColors {
    pub background: slint::Color,
    pub text: slint::Color,
    pub muted: slint::Color,
    pub line: slint::Color,
    pub link: slint::Color,
}

pub(crate) struct HtmlEditor {
    webview: Rc<WebView>,
    _follow: slint::Timer,
    visible: Rc<Cell<bool>>,
}

impl HtmlEditor {
    /// Runs one of the toolbar commands understood by `window.emailoo.cmd`.
    pub(crate) fn command(&self, name: &str) {
        self.call("cmd", &[serde_json::json!(name)]);
    }

    /// Gives keyboard focus back to the web view (after a toolbar click).
    pub(crate) fn focus(&self) {
        let _ = self.webview.focus();
    }

    /// Re-applies the theme colors without reloading the draft.
    pub(crate) fn set_theme(&self, colors: &EditorColors) {
        self.call(
            "setTheme",
            &[
                serde_json::json!(css(colors.background)),
                serde_json::json!(css(colors.text)),
                serde_json::json!(css(colors.muted)),
                serde_json::json!(css(colors.line)),
                serde_json::json!(css(colors.link)),
            ],
        );
    }

    /// Hides the native web view while a Slint dialog needs to be on top.
    /// The follow timer restores it when `visible` becomes true again.
    pub(crate) fn set_visible(&self, visible: bool) {
        self.visible.set(visible);
    }

    /// Calls `window.emailoo.<function>(args…)` in the page.
    pub(crate) fn call(&self, function: &str, args: &[serde_json::Value]) {
        let args = args
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let _ = self
            .webview
            .evaluate_script(&format!("window.emailoo.{function}({args})"));
    }
}

fn css(color: slint::Color) -> String {
    format!(
        "rgba({},{},{},{:.3})",
        color.red(),
        color.green(),
        color.blue(),
        f32::from(color.alpha()) / 255.0
    )
}

fn script(source: &str) -> String {
    source.replace("</script", "<\\/script")
}

fn page(initial_html: &str, colors: &EditorColors) -> String {
    let initial = serde_json::to_string(initial_html)
        .unwrap_or_else(|_| "\"\"".to_owned())
        .replace("</", "<\\/");
    TEMPLATE
        .replace("__BG__", &css(colors.background))
        .replace("__FG__", &css(colors.text))
        .replace("__MUTED__", &css(colors.muted))
        .replace("__LINE__", &css(colors.line))
        .replace("__LINK__", &css(colors.link))
        .replace("__DOMPURIFY__", &script(PURIFY))
        .replace("__SQUIRE__", &script(SQUIRE))
        // Last, so nothing inside the message is taken for a placeholder.
        .replace("__INITIAL__", &initial)
}

fn bounds(window: &ComposeWindow) -> (f32, f32, f32, f32) {
    (
        window.get_editor_x(),
        window.get_editor_y(),
        window.get_editor_width().max(1.0),
        window.get_editor_height().max(1.0),
    )
}

fn rect((x, y, width, height): (f32, f32, f32, f32)) -> Rect {
    Rect {
        position: LogicalPosition::new(f64::from(x), f64::from(y)).into(),
        size: LogicalSize::new(f64::from(width), f64::from(height)).into(),
    }
}

/// Adds a standard Edit menu so Cmd+C / Cmd+V / Cmd+Z reach the web view
/// (WKWebView only receives them through menu key equivalents).
fn ensure_edit_menu() {
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2::sel;
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::NSString;

    let Some(mtm) = MainThreadMarker::new() else { return };
    #[allow(unused_unsafe)]
    unsafe {
        let app = NSApplication::sharedApplication(mtm);
        let Some(main) = app.mainMenu() else { return };
        let title = NSString::from_str("Edición");
        if main.itemWithTitle(&title).is_some() {
            return;
        }
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &title);
        for (label, action, key) in [
            ("Deshacer", sel!(undo:), "z"),
            ("Rehacer", sel!(redo:), "Z"),
            ("Cortar", sel!(cut:), "x"),
            ("Copiar", sel!(copy:), "c"),
            ("Pegar", sel!(paste:), "v"),
            ("Seleccionar todo", sel!(selectAll:), "a"),
        ] {
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(label),
                Some(action),
                &NSString::from_str(key),
            );
            menu.addItem(&item);
        }
        let holder = NSMenuItem::new(mtm);
        holder.setTitle(&title);
        holder.setSubmenu(Some(&menu));
        main.addItem(&holder);
    }
}

enum Attach {
    Ready(HtmlEditor),
    /// Slint creates the native window lazily after `show()`.
    NotYet,
    Failed,
}

/// Messages posted by the page: `kind` and its JSON `value`.
pub(crate) type OnMessage = Rc<dyn Fn(&str, &serde_json::Value)>;

fn try_attach(
    window: &ComposeWindow,
    html: &str,
    colors: &EditorColors,
    on_message: &OnMessage,
) -> Attach {
    let initial = bounds(window);
    let built = window.window().with_winit_window(|winit_window| {
        WebViewBuilder::new()
            .with_html(page(html, colors))
            .with_bounds(rect(initial))
            .with_focused(true)
            // Links inside the draft must not replace the editor.
            .with_navigation_handler(|url| url.starts_with("about:"))
            .with_ipc_handler({
                let on_message = Rc::clone(on_message);
                move |request| {
                    if let Ok(message) = serde_json::from_str::<serde_json::Value>(request.body()) {
                        let kind = message["kind"].as_str().unwrap_or_default();
                        on_message(kind, &message["value"]);
                    }
                }
            })
            .build_as_child(winit_window)
    });
    let webview = match built {
        None => return Attach::NotYet,
        Some(Ok(webview)) => Rc::new(webview),
        Some(Err(error)) => {
            eprintln!("emailOO: HTML editor unavailable: {error}");
            return Attach::Failed;
        }
    };

    // Follow the editor area while the window is resized.
    let follow = slint::Timer::default();
    let last = Cell::new(initial);
    let visible = Rc::new(Cell::new(true));
    let visible_for_follow = Rc::clone(&visible);
    let window_weak = window.as_weak();
    let webview_weak = Rc::downgrade(&webview);
    follow.start(slint::TimerMode::Repeated, Duration::from_millis(60), move || {
        let (Some(window), Some(webview)) = (window_weak.upgrade(), webview_weak.upgrade()) else {
            return;
        };
        if !visible_for_follow.get() {
            if last.get() != (0.0, 0.0, 0.0, 0.0) {
                last.set((0.0, 0.0, 0.0, 0.0));
                let _ = webview.set_bounds(rect((0.0, 0.0, 0.0, 0.0)));
            }
            return;
        }
        let now = bounds(&window);
        if now != last.get() {
            last.set(now);
            let _ = webview.set_bounds(rect(now));
        }
    });
    Attach::Ready(HtmlEditor { webview, _follow: follow, visible })
}

/// Lays the editor over `window`'s editor area once its native window
/// exists, storing it in `slot`. Falls back to the plain TextEdit on failure.
pub(crate) fn attach(
    window: &ComposeWindow,
    html: String,
    colors: EditorColors,
    slot: Rc<RefCell<Option<HtmlEditor>>>,
    on_message: OnMessage,
) {
    ensure_edit_menu();
    attach_attempt(window.as_weak(), Rc::new((html, colors, on_message)), slot, 0);
}

fn attach_attempt(
    weak: slint::Weak<ComposeWindow>,
    content: Rc<(String, EditorColors, OnMessage)>,
    slot: Rc<RefCell<Option<HtmlEditor>>>,
    attempt: u32,
) {
    let Some(window) = weak.upgrade() else { return };
    match try_attach(&window, &content.0, &content.1, &content.2) {
        Attach::Ready(editor) => *slot.borrow_mut() = Some(editor),
        Attach::NotYet if attempt < 100 => {
            slint::Timer::single_shot(Duration::from_millis(30), move || {
                attach_attempt(weak, content, slot, attempt + 1);
            });
        }
        _ => window.set_html_editor(false),
    }
}
