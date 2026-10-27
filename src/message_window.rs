//! emailOO: open a conversation's latest message in its own window
//! (double-click in the message list).

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
    let date = Local
        .timestamp_opt(detail.date, 0)
        .single()
        .map(|date| date.format("%d/%m/%Y %H:%M").to_string())
        .unwrap_or_default();
    window.set_date(date.into());

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

/// Visual preview of composing in its own window (issue #3). Nothing is
/// connected: "Open in the composer" hands over to the real composer.
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

fn open_compose_preview(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    message: &MessageWindow,
    detail: &flectar_mail_core::models::MessageDetail,
    action: &str,
    thread_id: i32,
    windows: &Rc<RefCell<Vec<ComposeWindow>>>,
) -> bool {
    let Ok(window) = ComposeWindow::new() else { return false };
    let theme = app.global::<AppTheme>();
    window.set_page_color(theme.get_surface());
    window.set_text_color(theme.get_heading());
    window.set_muted_color(theme.get_secondary());
    window.set_line_color(theme.get_border_subtle());
    window.set_hover_color(theme.get_muted_control_bg());
    window.set_body_width(BODY_WIDTH as f32);
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
    let original = match (&detail.text_body, &detail.html_body) {
        (Some(text), _) if !text.trim().is_empty() => text.clone(),
        (_, Some(html)) => crate::email_document::fallback(html),
        _ => detail.snippet.clone(),
    };
    window.set_body(
        format!(
            "\n\n------ Mensaje original ------\nDe: {sender}\nPara: {recipients}\nFecha: {}\nAsunto: {}\n\n{}",
            message.get_date(),
            detail.subject,
            original.trim_end()
        )
        .into(),
    );
    {
        let app_weak = app.as_weak();
        let self_weak = window.as_weak();
        let action = SharedString::from(action);
        window.on_open_in_main_composer(move || {
            if let Some(window) = self_weak.upgrade() {
                let _ = window.hide();
            }
            let Some(app) = app_weak.upgrade() else { return };
            app.invoke_select_email(thread_id);
            app.invoke_compose_message_action(action.clone());
            let _ = app.show();
            app.window().with_winit_window(|window| window.focus_window());
        });
    }
    if window.show().is_err() {
        return false;
    }
    windows.borrow_mut().push(window);
    true
}

pub(super) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
) {
    // Open windows stay alive until the app quits.
    let windows = Rc::new(RefCell::new(Vec::<MessageWindow>::new()));
    let compose_windows = Rc::new(RefCell::new(Vec::<ComposeWindow>::new()));
    let weak = app.as_weak();
    let state = Rc::clone(state);
    let runtime = Rc::clone(runtime);
    app.global::<MessageWindowApi>().on_open(move |thread_id| {
        let Some(app) = weak.upgrade() else { return };
        let Some(core) = state
            .borrow()
            .core
            .as_ref()
            .map(|core| core.account_preferences_core())
        else {
            return;
        };
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
            // Reply / forward open the composer in the main window; the other
            // actions apply to this conversation and close the window.
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
                    && open_compose_preview(&app, &state, &runtime, &message, detail, action.as_str(), thread_id, &compose_windows)
                {
                    let _ = message.hide();
                    return;
                }
                match action.as_str() {
                    "reply" | "reply_all" | "forward" => {
                        // The composer lives in the main window: close this one
                        // and bring the main window forward with the draft open.
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
        let task = runtime.spawn(async move { core.get_latest_thread_body(i64::from(thread_id)).await });
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
