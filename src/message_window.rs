//! emailOO: open a conversation's latest message in its own window
//! (double-click in the message list), and compose replies / forwards in a
//! separate window.

use super::*;
use crate::message_appearance::EmailAppearance;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

const BODY_WIDTH: u32 = 760;

fn address(address: &flectar_mail_core::models::Address) -> String {
    match address.name.as_deref().map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => format!("{name} <{}>", address.email),
        None => address.email.clone(),
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn body_html(detail: &flectar_mail_core::models::MessageDetail) -> String {
    match (&detail.html_body, &detail.text_body) {
        (Some(html), _) if !html.trim().is_empty() => html.clone(),
        (_, Some(text)) => format!(
            "<pre style=\"white-space:pre-wrap;font-family:-apple-system,sans-serif;font-size:14px\">{}</pre>",
            escape_html(text)
        ),
        _ => format!("<p>{}</p>", escape_html(&detail.snippet)),
    }
}

fn date_label(detail: &flectar_mail_core::models::MessageDetail) -> String {
    Local
        .timestamp_millis_opt(detail.date)
        .single()
        .map(|date| date.format("%d/%m/%Y %H:%M").to_string())
        .unwrap_or_default()
}

/// The message-list row id → the conversation it belongs to.
fn conversation_for(state: &Rc<RefCell<InboxState>>, row_id: i32) -> i64 {
    state
        .borrow()
        .messages
        .iter()
        .find(|row| row.id == row_id)
        .and_then(|row| row.thread_id)
        .unwrap_or(i64::from(row_id))
}

fn fill(window: &MessageWindow, app: &AppWindow, detail: &flectar_mail_core::models::MessageDetail) {
    window.set_subject(detail.subject.clone().into());
    window.set_starred(detail.is_starred);
    window.set_unread(!detail.is_read);
    window.set_sender(address(&detail.from).into());
    let recipients = detail
        .to
        .iter()
        .chain(detail.cc.iter())
        .map(address)
        .collect::<Vec<_>>()
        .join(", ");
    window.set_recipients(recipients.into());
    window.set_date(date_label(detail).into());

    let scale = app.window().scale_factor();
    match renderer::render_email_document(
        &body_html(detail),
        BODY_WIDTH,
        scale,
        EmailAppearance::from_app(app),
    ) {
        Ok(rendered) => {
            let tiles = rendered
                .tiles
                .into_iter()
                .map(|tile| WindowEmailTile {
                    image: tile.image,
                    y: tile.y,
                    height: tile.height,
                })
                .collect::<Vec<_>>();
            let logical_height = rendered.height as f32 / scale.max(1.0);
            window.set_content_ratio(logical_height / BODY_WIDTH as f32);
            window.set_tiles(ModelRc::new(VecModel::from(tiles)));
            window.set_status(SharedString::new());
        }
        Err(error) => window.set_status(format!("No se pudo mostrar el mensaje: {error}").into()),
    }
}

fn prefixed(prefix: &str, subject: &str) -> String {
    let lower = subject.trim_start().to_lowercase();
    if lower.starts_with(&prefix.to_lowercase()) {
        subject.to_owned()
    } else {
        format!("{prefix} {subject}")
    }
}

fn account_choices(state: &Rc<RefCell<InboxState>>, account_id: i64) -> (Vec<SharedString>, i32) {
    let state = state.borrow();
    let accounts = state
        .account_configs
        .iter()
        .map(|account| SharedString::from(account.email.as_str()))
        .collect::<Vec<_>>();
    let index = state
        .account_configs
        .iter()
        .position(|account| account.id == account_id)
        .unwrap_or(0) as i32;
    (accounts, index)
}

fn signature_names(
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    account_id: i64,
) -> Vec<SharedString> {
    let Some(core) = state.borrow().core.as_ref().map(|core| core.account_preferences_core()) else {
        return Vec::new();
    };
    runtime
        .block_on(core.get_settings())
        .map(|settings| {
            settings
                .signature_list
                .iter()
                .filter(|signature| signature.account_id == account_id)
                .map(|signature| SharedString::from(signature.name.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

/// A compose window and, on macOS, the HTML editor laid over it.
pub(super) struct ComposeHandle {
    _window: ComposeWindow,
    #[cfg(target_os = "macos")]
    _editor: Rc<RefCell<Option<crate::html_editor::HtmlEditor>>>,
}

fn open_compose_window(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    detail: &flectar_mail_core::models::MessageDetail,
    action: &str,
    windows: &Rc<RefCell<Vec<ComposeHandle>>>,
) -> bool {
    let Ok(window) = ComposeWindow::new() else { return false };
    let theme = app.global::<AppTheme>();
    window.set_page_color(theme.get_surface());
    window.set_text_color(theme.get_heading());
    window.set_muted_color(theme.get_secondary());
    window.set_line_color(theme.get_border_subtle());
    window.set_hover_color(theme.get_muted_control_bg());
    window.set_body_width(BODY_WIDTH as f32);
    let forward = !matches!(action, "reply" | "reply_all");
    let sender = address(&detail.from);
    let (to, cc, subject) = match action {
        "reply" => (sender.clone(), String::new(), prefixed("Re:", &detail.subject)),
        "reply_all" => (
            sender.clone(),
            detail.cc.iter().map(address).collect::<Vec<_>>().join(", "),
            prefixed("Re:", &detail.subject),
        ),
        _ => (String::new(), String::new(), prefixed("Fwd:", &detail.subject)),
    };
    window.set_to(to.into());
    window.set_cc(cc.into());
    window.set_subject(subject.into());
    let (accounts, account_index) = account_choices(state, detail.account_id);
    window.set_accounts(ModelRc::new(VecModel::from(accounts)));
    window.set_account_index(account_index);
    let mut signatures = vec![SharedString::from("Sin firma")];
    signatures.extend(signature_names(state, runtime, detail.account_id));
    window.set_signatures(ModelRc::new(VecModel::from(signatures)));
    let recipients = detail.to.iter().map(address).collect::<Vec<_>>().join(", ");
    let date = date_label(detail);
    let heading = if forward { "Mensaje reenviado" } else { "Mensaje original" };

    // Plain-text fallback (non-macOS, or if the web view cannot be created).
    let original = match (&detail.text_body, &detail.html_body) {
        (Some(text), _) if !text.trim().is_empty() => text.clone(),
        (_, Some(html)) => crate::email_document::fallback(html),
        _ => detail.snippet.clone(),
    };
    window.set_body(
        format!(
            "\n\n------ {heading} ------\nDe: {sender}\nPara: {recipients}\nFecha: {date}\nAsunto: {}\n\n{}",
            detail.subject,
            original.trim_end()
        )
        .into(),
    );

    #[cfg(target_os = "macos")]
    window.set_html_editor(true);
    window.on_send({
        let weak = window.as_weak();
        move || {
            if let Some(window) = weak.upgrade() {
                window.set_notice("Vista previa: esta ventana todavía no envía.".into());
            }
        }
    });
    if window.show().is_err() {
        return false;
    }

    #[cfg(target_os = "macos")]
    let editor = {
        let html = format!(
            "<div><br></div><div><br></div>\
             <div class=\"emailoo-header\">------ {heading} ------<br>De: {}<br>Para: {}<br>Fecha: {}<br>Asunto: {}</div>\
             <blockquote type=\"cite\">{}</blockquote>",
            escape_html(&sender),
            escape_html(&recipients),
            escape_html(&date),
            escape_html(&detail.subject),
            body_html(detail)
        );
        let colors = crate::html_editor::EditorColors {
            background: theme.get_surface(),
            text: theme.get_heading(),
            muted: theme.get_secondary(),
            line: theme.get_border_subtle(),
            link: theme.get_accent(),
        };
        let editor = Rc::new(RefCell::new(None::<crate::html_editor::HtmlEditor>));
        let on_message: crate::html_editor::OnMessage = {
            let weak = window.as_weak();
            let editor = Rc::downgrade(&editor);
            let state = Rc::clone(state);
            let runtime = Rc::clone(runtime);
            Rc::new(move |kind: &str, value: &serde_json::Value| {
                if kind != "ai" {
                    return;
                }
                let Some(window) = weak.upgrade() else { return };
                let index = value["preset"].as_u64().unwrap_or(u64::MAX) as usize;
                let text = value["text"].as_str().unwrap_or_default().to_owned();
                let Some((name, instruction)) = crate::ai_controller::preset_at(index) else { return };
                let Some(core) = state
                    .borrow()
                    .core
                    .as_ref()
                    .map(|core| core.account_preferences_core())
                else {
                    window.set_notice("IA: emailOO todavía se está iniciando.".into());
                    return;
                };
                window.set_notice(format!("IA · {name}…").into());
                let task = runtime.spawn(async move {
                    core.ai_assist(instruction, text)
                        .await
                        .map_err(|error| error.to_string())
                });
                let weak = weak.clone();
                let editor = editor.clone();
                let _ = slint::spawn_local(async move {
                    let result = task
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result);
                    let Some(window) = weak.upgrade() else { return };
                    match result {
                        Ok(answer) => {
                            if let Some(editor) = editor.upgrade()
                                && let Some(editor) = editor.borrow().as_ref()
                            {
                                editor.call("aiApply", &[serde_json::json!(answer.trim())]);
                            }
                            window.set_notice(SharedString::new());
                        }
                        Err(error) => window.set_notice(format!("IA: {error}").into()),
                    }
                });
            })
        };
        crate::html_editor::attach(&window, html, colors, Rc::clone(&editor), on_message);
        window.on_format({
            let editor = Rc::clone(&editor);
            let weak = window.as_weak();
            move |command| {
                let editor = editor.borrow();
                let Some(editor) = editor.as_ref() else { return };
                if command.as_str() == "ai" {
                    let left = weak.upgrade().map(|window| window.get_ai_button_x()).unwrap_or(0.0);
                    editor.call(
                        "showAiMenu",
                        &[
                            serde_json::json!(crate::ai_controller::preset_names()),
                            serde_json::json!(left),
                        ],
                    );
                } else {
                    editor.command(command.as_str());
                }
            }
        });
        // Closing the window removes the web view with it.
        window.window().on_close_requested({
            let editor = Rc::clone(&editor);
            move || {
                editor.borrow_mut().take();
                slint::CloseRequestResponse::HideWindow
            }
        });
        editor
    };

    windows.borrow_mut().push(ComposeHandle {
        _window: window,
        #[cfg(target_os = "macos")]
        _editor: editor,
    });
    true
}

thread_local! {
    // Compose windows stay alive until the app quits.
    static COMPOSE_WINDOWS: Rc<RefCell<Vec<ComposeHandle>>> = Rc::new(RefCell::new(Vec::new()));
}

fn compose_windows() -> Rc<RefCell<Vec<ComposeHandle>>> {
    COMPOSE_WINDOWS.with(Rc::clone)
}

/// Reply / reply all / forward from the main window: opens the separate
/// compose window for `message_id` (or the conversation's latest message).
pub(super) fn compose_for(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    row_id: i32,
    message_id: Option<i64>,
    action: &str,
) {
    let task = match message_id {
        Some(message_id) => {
            let Some(core) = state
                .borrow()
                .core
                .as_ref()
                .map(|core| core.account_preferences_core())
            else {
                return;
            };
            runtime.spawn(async move { core.get_body(message_id).await.map_err(|error| error.to_string()) })
        }
        None => {
            let Some(task) = load_detail(state, runtime, row_id) else { return };
            task
        }
    };
    let weak = app.as_weak();
    let state = Rc::clone(state);
    let runtime = Rc::clone(runtime);
    let action = action.to_owned();
    let _ = slint::spawn_local(async move {
        let Some(app) = weak.upgrade() else { return };
        match task.await {
            Ok(Ok(detail)) => {
                open_compose_window(&app, &state, &runtime, &detail, &action, &compose_windows());
            }
            Ok(Err(error)) => app.set_render_status(UiMessage::detail("Could not open composer: {}", error)),
            Err(error) => app.set_render_status(UiMessage::detail("Could not open composer: {}", error.to_string())),
        }
    });
}

/// Loads the latest message of the conversation behind `row_id`.
fn load_detail(
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    row_id: i32,
) -> Option<tokio::task::JoinHandle<Result<flectar_mail_core::models::MessageDetail, String>>> {
    let core = state
        .borrow()
        .core
        .as_ref()
        .map(|core| core.account_preferences_core())?;
    let conversation = conversation_for(state, row_id);
    Some(runtime.spawn(async move {
        core.get_latest_thread_body(conversation)
            .await
            .map_err(|error| error.to_string())
    }))
}

pub(super) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
) {
    // Open windows stay alive until the app quits.
    let windows = Rc::new(RefCell::new(Vec::<MessageWindow>::new()));
    let compose_windows = compose_windows();

    // Reply / reply all / forward from the message list's context menu.
    {
        let weak = app.as_weak();
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        app.global::<MessageWindowApi>().on_compose(move |row_id, action| {
            let Some(app) = weak.upgrade() else { return };
            compose_for(&app, &state, &runtime, row_id, None, action.as_str());
        });
    }

    let weak = app.as_weak();
    let state = Rc::clone(state);
    let runtime = Rc::clone(runtime);
    app.global::<MessageWindowApi>().on_open(move |thread_id| {
        let Some(app) = weak.upgrade() else { return };
        let Some(task) = load_detail(&state, &runtime, thread_id) else { return };
        let Ok(window) = MessageWindow::new() else { return };
        let loaded = Rc::new(RefCell::new(None::<flectar_mail_core::models::MessageDetail>));
        let theme = app.global::<AppTheme>();
        window.set_page_color(theme.get_surface());
        window.set_text_color(theme.get_heading());
        window.set_muted_color(theme.get_secondary());
        window.set_line_color(theme.get_border_subtle());
        window.set_hover_color(theme.get_muted_control_bg());
        window.set_body_width(BODY_WIDTH as f32);
        window.set_status("Cargando…".into());
        {
            // Reply / forward open the compose window; the other actions
            // apply to this conversation and close the window.
            let app_weak = app.as_weak();
            let self_weak = window.as_weak();
            let loaded = Rc::clone(&loaded);
            let compose_windows = Rc::clone(&compose_windows);
            let state = Rc::clone(&state);
            let runtime = Rc::clone(&runtime);
            window.on_action(move |action| {
                let Some(app) = app_weak.upgrade() else { return };
                if matches!(action.as_str(), "reply" | "reply_all" | "forward")
                    && let (Some(message), Some(detail)) =
                        (self_weak.upgrade(), loaded.borrow().as_ref())
                    && open_compose_window(&app, &state, &runtime, detail, action.as_str(), &compose_windows)
                {
                    let _ = message.hide();
                    return;
                }
                match action.as_str() {
                    "reply" | "reply_all" | "forward" => {
                        // Fallback: the composer in the main window.
                        if let Some(window) = self_weak.upgrade() {
                            let _ = window.hide();
                        }
                        app.invoke_select_email(thread_id);
                        app.invoke_compose_message_action(action);
                        let _ = app.show();
                        app.window().with_winit_window(|window| window.focus_window());
                    }
                    "labels" | "print" | "browser" | "openpgp" => {
                        // These live in the main reader: show the message there.
                        app.invoke_select_email(thread_id);
                        let _ = app.show();
                        app.window().with_winit_window(|window| window.focus_window());
                        match action.as_str() {
                            "labels" => {
                                let requests = app.global::<ReaderRequests>();
                                requests.set_labels(requests.get_labels().wrapping_add(1));
                            }
                            "print" => app.invoke_print_message(),
                            "browser" => app.invoke_open_message_in_browser(),
                            _ => app.global::<AccountMailPreferences>().invoke_read_message(),
                        }
                    }
                    _ => {
                        app.invoke_message_action_for_email(thread_id, action.clone());
                        if matches!(action.as_str(), "archive" | "spam" | "trash")
                            && let Some(window) = self_weak.upgrade()
                        {
                            let _ = window.hide();
                        }
                    }
                }
            });
        }
        if window.show().is_err() {
            return;
        }
        let window_weak = window.as_weak();
        windows.borrow_mut().push(window);

        let loaded_for_fill = Rc::clone(&loaded);
        let app_weak = app.as_weak();
        let _ = slint::spawn_local(async move {
            let result = task.await.map_err(|error| error.to_string());
            let (Some(window), Some(app)) = (window_weak.upgrade(), app_weak.upgrade()) else {
                return;
            };
            match result {
                Ok(Ok(detail)) => {
                    fill(&window, &app, &detail);
                    *loaded_for_fill.borrow_mut() = Some(detail);
                }
                Ok(Err(error)) => {
                    window.set_status(format!("No se pudo cargar el mensaje: {error}").into())
                }
                Err(error) => window.set_status(error.into()),
            }
        });
    });
}
