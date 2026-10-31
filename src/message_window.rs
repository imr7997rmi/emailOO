//! emailOO: open a conversation's latest message in its own window
//! (double-click in the message list), and compose replies / forwards in a
//! separate window.

use super::*;
use crate::message_appearance::EmailAppearance;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::time::Duration;

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

/// Extension → MIME type for images dropped into the draft body. WKWebView
/// accepts a data URI, so no temporary file is needed.
fn image_mime(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("svg") => Some("image/svg+xml"),
        _ => None,
    }
}

/// Decode an image file for the in-window attachment preview. Slint without
/// its `image` feature cannot load paths, so the workspace decoder does it.
fn slint_image_from_path(path: &std::path::Path) -> Option<slint::Image> {
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let image = reader.decode().ok()?.thumbnail(1200, 900).into_rgba8();
    let (width, height) = image.dimensions();
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(image.as_raw());
    Some(slint::Image::from_rgba8(buffer))
}

/// One decimal place, as in "12.4 MB". Used in the Nextcloud dialog and link.
fn file_size_mb(path: &std::path::Path) -> f64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}

/// True when `path` is inside `folder` (both canonicalized so symlinks match).
fn path_inside(folder: &str, path: &std::path::Path) -> bool {
    let folder = folder.trim();
    if folder.is_empty() {
        return false;
    }
    let Ok(folder) = std::path::Path::new(folder).canonicalize() else {
        return false;
    };
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    path.starts_with(folder)
}

fn contact_rows(contacts: &[flectar_mail_core::models::Address]) -> Vec<ComposeContact> {
    contacts
        .iter()
        .map(|contact| ComposeContact {
            name: contact.name.clone().unwrap_or_default().into(),
            email: contact.email.clone().into(),
            initials: crate::compose_controller::contact_initials(contact).into(),
        })
        .collect()
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


fn signature_choices(
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    account_id: i64,
) -> Vec<(String, String)> {
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
                .map(|signature| (signature.name.clone(), signature.html.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn sender_choices(
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    account_id: i64,
) -> (Vec<SharedString>, i32, Vec<flectar_mail_core::models::SenderIdentity>) {
    let Some(core) = state.borrow().core.as_ref().map(|core| core.account_preferences_core()) else {
        return (Vec::new(), 0, Vec::new());
    };
    let identities = runtime
        .block_on(core.list_sender_identities(account_id))
        .unwrap_or_default();
    let default_email = runtime
        .block_on(core.default_sender_identity(account_id))
        .map(|identity| identity.email.to_lowercase())
        .unwrap_or_default();
    let names = identities
        .iter()
        .map(|identity| {
            SharedString::from(
                identity
                    .display_name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| identity.email.clone()),
            )
        })
        .collect::<Vec<_>>();
    let index = identities
        .iter()
        .position(|identity| identity.email.eq_ignore_ascii_case(&default_email))
        .unwrap_or(0) as i32;
    (names, index, identities)
}

struct ComposeAttachmentFile {
    path: std::path::PathBuf,
    filename: String,
}

fn attachment_size_label(path: &std::path::Path) -> String {
    let Ok(metadata) = std::fs::metadata(path) else {
        return String::new();
    };
    let size = metadata.len();
    if size >= 1024 * 1024 {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{:.0} KB", size as f64 / 1024.0)
    } else {
        format!("{size} B")
    }
}

fn apply_compose_attachments(window: &ComposeWindow, files: &[ComposeAttachmentFile]) {
    let rows = files
        .iter()
        .map(|file| ComposeAttachment {
            name: file.filename.clone().into(),
            detail: attachment_size_label(&file.path).into(),
        })
        .collect::<Vec<_>>();
    window.set_attachments(ModelRc::new(VecModel::from(rows)));
}

/// A separate Slint window gets its own `AppTheme` and `Palette` instances.
/// Copy the persisted palette and color scheme so composing follows the app.
fn copy_theme(app: &AppWindow, window: &ComposeWindow) {
    let source = app.global::<AppTheme>();
    let target = window.global::<AppTheme>();
    target.set_preset(source.get_preset());
    target.set_custom_light_primary(source.get_custom_light_primary());
    target.set_custom_light_page_bg(source.get_custom_light_page_bg());
    target.set_custom_light_surface(source.get_custom_light_surface());
    target.set_custom_light_text(source.get_custom_light_text());
    target.set_custom_light_border(source.get_custom_light_border());
    target.set_custom_dark_primary(source.get_custom_dark_primary());
    target.set_custom_dark_page_bg(source.get_custom_dark_page_bg());
    target.set_custom_dark_surface(source.get_custom_dark_surface());
    target.set_custom_dark_text(source.get_custom_dark_text());
    target.set_custom_dark_border(source.get_custom_dark_border());
    window.set_theme_mode(app.get_theme_mode());
}

/// Keep every open compose window in step after a theme change.
pub(super) fn refresh_theme(app: &AppWindow) {
    for handle in compose_windows().borrow().iter() {
        copy_theme(app, &handle._window);
    }
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
    detail: Option<&flectar_mail_core::models::MessageDetail>,
    action: &str,
    windows: &Rc<RefCell<Vec<ComposeHandle>>>,
) -> bool {
    let Ok(window) = ComposeWindow::new() else { return false };
    copy_theme(app, &window);
    let theme = app.global::<AppTheme>();
    window.set_nextcloud_local_path(app.get_nextcloud_local_path());
    window.set_body_width(BODY_WIDTH as f32);
    let forward = action == "forward";
    let is_new = action == "new";
    let empty_address = flectar_mail_core::models::Address {
        name: None,
        email: String::new(),
    };
    let from = detail.map(|detail| &detail.from).unwrap_or(&empty_address);
    let detail_to = detail.map(|detail| detail.to.as_slice()).unwrap_or(&[]);
    let detail_cc = detail.map(|detail| detail.cc.as_slice()).unwrap_or(&[]);
    let detail_subject = detail.map(|detail| detail.subject.as_str()).unwrap_or("");
    let account_id = detail
        .map(|detail| detail.account_id)
        .or_else(|| state.borrow().account_configs.first().map(|account| account.id))
        .unwrap_or(0);
    let sender = address(from);
    let (to, cc, subject) = match action {
        "reply" => (sender.clone(), String::new(), prefixed("Re:", detail_subject)),
        "reply_all" => {
            // Everyone else on the message, without our own addresses.
            let own = state
                .borrow()
                .account_configs
                .iter()
                .map(|account| account.email.to_lowercase())
                .collect::<Vec<_>>();
            let others = detail_to
                .iter()
                .chain(detail_cc.iter())
                .filter(|other| {
                    let email = other.email.to_lowercase();
                    !own.contains(&email) && email != from.email.to_lowercase()
                })
                .map(address)
                .collect::<Vec<_>>()
                .join(", ");
            (sender.clone(), others, prefixed("Re:", detail_subject))
        }
        "" | "new" => (String::new(), String::new(), String::new()),
        _ => (String::new(), String::new(), prefixed("Fwd:", detail_subject)),
    };
    window.set_to(to.into());
    window.set_cc(cc.into());
    window.set_subject(subject.into());
    let accounts = app.get_connected_accounts();
    let account_index = (0..accounts.row_count())
        .find(|index| {
            accounts
                .row_data(*index)
                .is_some_and(|account| i64::from(account.id) == account_id)
        })
        .unwrap_or(0) as i32;
    window.set_accounts(accounts);
    window.set_account_index(account_index);
    let signature_options = signature_choices(state, runtime, account_id);
    let mut signatures = vec![SharedString::from("Sin firma")];
    signatures.extend(
        signature_options
            .iter()
            .map(|(name, _)| SharedString::from(name.as_str())),
    );
    window.set_signatures(ModelRc::new(VecModel::from(signatures)));
    let signature_html = Rc::new(
        signature_options
            .iter()
            .map(|(_, html)| html.clone())
            .collect::<Vec<_>>(),
    );
    let sender_identities = Rc::new(RefCell::new(Vec::<flectar_mail_core::models::SenderIdentity>::new()));
    {
        let (names, index, identities) = sender_choices(state, runtime, account_id);
        window.set_sender_names(ModelRc::new(VecModel::from(names)));
        window.set_sender_index(index);
        *sender_identities.borrow_mut() = identities;
    }
    {
        let weak = window.as_weak();
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        let identities = Rc::clone(&sender_identities);
        window.on_account_selected(move |index| {
            let Some(window) = weak.upgrade() else { return };
            let Some(account_id) = window
                .get_accounts()
                .row_data(usize::try_from(index).unwrap_or(0))
                .map(|account| i64::from(account.id))
            else {
                return;
            };
            let (names, sender_index, loaded) = sender_choices(&state, &runtime, account_id);
            window.set_sender_names(ModelRc::new(VecModel::from(names)));
            window.set_sender_index(sender_index);
            *identities.borrow_mut() = loaded;
        });
    }
    let attachment_files = Rc::new(RefCell::new(Vec::<ComposeAttachmentFile>::new()));
    {
        let weak = window.as_weak();
        let files = Rc::clone(&attachment_files);
        window.on_pick_attachments(move || {
            let Some(window) = weak.upgrade() else { return };
            let picked = rfd::FileDialog::new().set_title("Attach files").pick_files();
            let Some(picked) = picked else { return };
            let mut files = files.borrow_mut();
            for path in picked {
                if files.iter().any(|file| file.path == path) {
                    continue;
                }
                let Some(filename) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
                    continue;
                };
                files.push(ComposeAttachmentFile { path, filename });
            }
            apply_compose_attachments(&window, &files);
        });
    }
    {
        let weak = window.as_weak();
        let files = Rc::clone(&attachment_files);
        window.on_remove_attachment(move |index| {
            let Some(window) = weak.upgrade() else { return };
            let Ok(index) = usize::try_from(index) else { return };
            let mut files = files.borrow_mut();
            if index < files.len() {
                files.remove(index);
            }
            apply_compose_attachments(&window, &files);
        });
    }

    // Recipient suggestions from the address book, as in the main composer.
    let suggestions = Rc::new(RefCell::new(Vec::<flectar_mail_core::models::Address>::new()));
    {
        let weak = window.as_weak();
        let suggestions = Rc::clone(&suggestions);
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        window.on_search_contacts(move |recipients, account_index| {
            let Some(window) = weak.upgrade() else { return };
            let query = crate::compose_controller::compose_recipient_query(recipients.as_str());
            let mut contacts = Vec::new();
            if !query.is_empty() {
                let (core, account_id) = {
                    let state = state.borrow();
                    let core = state
                        .core
                        .as_ref()
                        .map(|core| core.account_preferences_core());
                    let account_id = state
                        .account_configs
                        .get(usize::try_from(account_index).unwrap_or(0))
                        .map(|account| account.id);
                    (core, account_id)
                };
                if let Some(core) = core
                    && let Ok(mut found) =
                        runtime.block_on(core.list_contacts(query, account_id, 6))
                {
                    let existing = recipients.to_lowercase();
                    found.retain(|contact| !existing.contains(&contact.email.to_lowercase()));
                    contacts = found;
                }
            }
            let rows = contact_rows(&contacts);
            *suggestions.borrow_mut() = contacts;
            window.set_contact_suggestions(ModelRc::new(VecModel::from(rows)));
            window.set_highlighted_contact(0);
        });
    }
    {
        let weak = window.as_weak();
        let suggestions = Rc::clone(&suggestions);
        window.on_select_contact(move |index, current_recipients| {
            let Ok(index) = usize::try_from(index) else {
                return current_recipients;
            };
            let Some(contact) = suggestions.borrow().get(index).cloned() else {
                return current_recipients;
            };
            let recipients =
                crate::compose_controller::complete_compose_recipient(current_recipients.as_str(), &contact);
            suggestions.borrow_mut().clear();
            if let Some(window) = weak.upgrade() {
                window.set_contact_suggestions(ModelRc::new(VecModel::from(Vec::<ComposeContact>::new())));
            }
            recipients.into()
        });
    }
    if let Some(detail) = detail {
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
    }

    #[cfg(target_os = "macos")]
    window.set_html_editor(true);
    // Sending goes through the same path as the built-in composer:
    // save a draft, queue it and send it now.
    let mode = if is_new {
        "new".to_owned()
    } else if forward {
        "forward".to_owned()
    } else {
        action.to_owned()
    };
    let in_reply_to = (!forward && !is_new)
        .then(|| detail.map(|detail| detail.id))
        .flatten();
    let send: Rc<dyn Fn(String, Option<String>)> = {
        let weak = window.as_weak();
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        let attachment_files = Rc::clone(&attachment_files);
        let sender_identities = Rc::clone(&sender_identities);
        Rc::new(move |text: String, html: Option<String>| {
            let Some(window) = weak.upgrade() else { return };
            let Some(core) = state.borrow().core.clone() else {
                window.set_notice("Conecta una cuenta antes de enviar.".into());
                return;
            };
            let selected_id = window
                .get_accounts()
                .row_data(usize::try_from(window.get_account_index()).unwrap_or(0))
                .map(|account| i64::from(account.id));
            let account = selected_id.and_then(|id| {
                state
                    .borrow()
                    .account_configs
                    .iter()
                    .find(|account| account.id == id)
                    .map(|account| (account.id, account.email.clone()))
            });
            let Some((account_id, account_email)) = account else {
                window.set_notice("Elige la cuenta desde la que enviar.".into());
                return;
            };
            let sender_email = sender_identities
                .borrow()
                .get(usize::try_from(window.get_sender_index()).unwrap_or(0))
                .map(|identity| identity.email.clone())
                .filter(|email| !email.trim().is_empty())
                .unwrap_or(account_email);
            let (to, cc, bcc, subject) = (
                window.get_to().to_string(),
                window.get_cc().to_string(),
                window.get_bcc().to_string(),
                window.get_subject().to_string(),
            );
            let mode = mode.clone();
            let attachments = attachment_files
                .borrow()
                .iter()
                .map(|file| flectar_mail_core::models::DraftAttachmentIn {
                    file_path: file.path.to_string_lossy().into_owned(),
                    filename: file.filename.clone(),
                })
                .collect::<Vec<_>>();
            window.set_notice_error(false);
            window.set_notice("Enviando…".into());
            let task = runtime.spawn(async move {
                let queued = core
                    .send_new_message(crate::mail::ComposeMessage {
                        draft_id: None,
                        account_id,
                        sender_email: Some(sender_email.as_str()),
                        to: to.as_str(),
                        cc: cc.as_str(),
                        bcc: bcc.as_str(),
                        subject: subject.as_str(),
                        body: text.as_str(),
                        body_html: html.as_deref(),
                        attachments: &attachments,
                        mode: mode.as_str(),
                        in_reply_to_message_id: in_reply_to,
                    })
                    .await?;
                core.wait_for_send(queued.action_id).await
            });
            let weak = weak.clone();
            let _ = slint::spawn_local(async move {
                let result = task.await.map_err(|error| error.to_string()).and_then(|result| result);
                let Some(window) = weak.upgrade() else { return };
                match result {
                    Ok(()) => {
                        window.set_notice(SharedString::new());
                        // Same as closing it: the web view goes away too.
                        window.window().dispatch_event(slint::platform::WindowEvent::CloseRequested);
                    }
                    Err(error) => {
                        window.set_notice_error(true);
                        window.set_notice(format!("No se pudo enviar: {error}").into());
                    }
                }
            });
        })
    };
    window.on_send({
        let weak = window.as_weak();
        let send = Rc::clone(&send);
        move || {
            if let Some(window) = weak.upgrade() {
                send(window.get_body().to_string(), None);
            }
        }
    });
    if window.show().is_err() {
        return false;
    }

    #[cfg(target_os = "macos")]
    let editor = {
        let html = match detail {
            Some(detail) => {
                let recipients = detail.to.iter().map(address).collect::<Vec<_>>().join(", ");
                let date = date_label(detail);
                let heading = if forward { "Mensaje reenviado" } else { "Mensaje original" };
                format!(
                    "<div><br></div><div><br></div>\
                     <div class=\"emailoo-header\">------ {heading} ------<br>De: {}<br>Para: {}<br>Fecha: {}<br>Asunto: {}</div>\
                     <blockquote type=\"cite\">{}</blockquote>",
                    escape_html(&sender),
                    escape_html(&recipients),
                    escape_html(&date),
                    escape_html(&detail.subject),
                    body_html(detail)
                )
            }
            None => "<div><br></div>".to_owned(),
        };
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
            let send = Rc::clone(&send);
            Rc::new(move |kind: &str, value: &serde_json::Value| {
                let Some(window) = weak.upgrade() else { return };
                if kind == "pointer" {
                    // Clears hover (and tooltips) on the Slint toolbar.
                    window
                        .window()
                        .dispatch_event(slint::platform::WindowEvent::PointerExited);
                    return;
                }
                if kind == "send" {
                    let text = value["text"].as_str().unwrap_or_default().to_owned();
                    let html = value["html"].as_str().map(str::to_owned);
                    send(text, html);
                    return;
                }
                if kind != "ai" {
                    return;
                }
                let show = |function: &str, text: String| {
                    if let Some(editor) = editor.upgrade()
                        && let Some(editor) = editor.borrow().as_ref()
                    {
                        editor.call(function, &[serde_json::json!(text)]);
                    }
                };
                let index = value["preset"].as_u64().unwrap_or(u64::MAX) as usize;
                let text = value["text"].as_str().unwrap_or_default().to_owned();
                let Some((name, instruction)) = crate::ai_controller::preset_at(index) else { return };
                let Some(core) = state
                    .borrow()
                    .core
                    .as_ref()
                    .map(|core| core.account_preferences_core())
                else {
                    show("aiError", "IA: emailOO todavía se está iniciando.".to_owned());
                    return;
                };
                show("aiBusy", format!("IA · {name}…"));
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
                    // The pointer is usually over the editor by now; drop the
                    // AI button's hover so its tooltip does not stay up.
                    window
                        .window()
                        .dispatch_event(slint::platform::WindowEvent::PointerExited);
                    match result {
                        Ok(answer) => {
                            if let Some(editor) = editor.upgrade()
                                && let Some(editor) = editor.borrow().as_ref()
                            {
                                editor.call("aiApply", &[serde_json::json!(answer.trim())]);
                            }
                        }
                        Err(error) => {
                            if let Some(editor) = editor.upgrade()
                                && let Some(editor) = editor.borrow().as_ref()
                            {
                                editor.call("aiError", &[serde_json::json!(format!("IA: {error}"))]);
                            }
                        }
                    }
                });
            })
        };
        crate::html_editor::attach(&window, html, colors, Rc::clone(&editor), on_message);
        {
            let weak = window.as_weak();
            let app = app.as_weak();
            let editor = Rc::clone(&editor);
            window.on_theme_changed(move || {
                if weak.upgrade().is_none() {
                    return;
                }
                let Some(app) = app.upgrade() else { return };
                let theme = app.global::<AppTheme>();
                let colors = crate::html_editor::EditorColors {
                    background: theme.get_surface(),
                    text: theme.get_heading(),
                    muted: theme.get_secondary(),
                    line: theme.get_border_subtle(),
                    link: theme.get_accent(),
                };
                if let Some(editor) = editor.borrow().as_ref() {
                    editor.set_theme(&colors);
                }
            });
        }
        {
            let editor = Rc::clone(&editor);
            let signature_html = Rc::clone(&signature_html);
            window.on_signature_selected(move |index| {
                let html = usize::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| signature_html.get(index))
                    .cloned()
                    .unwrap_or_default();
                if let Some(editor) = editor.borrow().as_ref() {
                    editor.call("insertSignature", &[serde_json::json!(html)]);
                }
            });
        }
        {
            let editor = Rc::clone(&editor);
            let weak = window.as_weak();
            let state = Rc::clone(state);
            let runtime = Rc::clone(runtime);
            window.on_insert_nextcloud_link(move || {
                let Some(window) = weak.upgrade() else { return };
                let Some(path) = rfd::FileDialog::new()
                    .set_title("Insert a Nextcloud share link")
                    .pick_file()
                else {
                    return;
                };
                let Some(core) = state
                    .borrow()
                    .core
                    .as_ref()
                    .map(|core| core.account_preferences_core())
                else {
                    window.set_notice("Conecta una cuenta antes de insertar enlaces.".into());
                    return;
                };
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let path = path.to_string_lossy().into_owned();
                let task = runtime.spawn(async move {
                    core.create_nextcloud_share(path, None, None)
                        .await
                        .map_err(|error| error.to_string())
                });
                let weak = weak.clone();
                let editor = Rc::clone(&editor);
                let _ = slint::spawn_local(async move {
                    let result = task.await.map_err(|error| error.to_string()).and_then(|result| result);
                    let Some(window) = weak.upgrade() else { return };
                    match result {
                        Ok(url) => {
                            let link = format!(
                                "<p><a href=\"{}\">{}</a></p>",
                                escape_html(&url),
                                escape_html(if name.is_empty() { &url } else { &name })
                            );
                            if let Some(editor) = editor.borrow().as_ref() {
                                editor.call("insertHtml", &[serde_json::json!(link)]);
                            }
                        }
                        Err(error) => {
                            window.set_notice_error(true);
                            window.set_notice(format!("Nextcloud: {error}").into());
                        }
                    }
                });
            });
        }
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
                    editor.focus();
                } else {
                    editor.command(command.as_str());
                    editor.focus();
                }
            }
        });
        window.on_send({
            let editor = Rc::clone(&editor);
            let weak = window.as_weak();
            let send = Rc::clone(&send);
            move || match editor.borrow().as_ref() {
                Some(editor) => editor.call("requestSend", &[]),
                None => {
                    if let Some(window) = weak.upgrade() {
                        send(window.get_body().to_string(), None);
                    }
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

        {
            let weak = window.as_weak();
            let files = Rc::clone(&attachment_files);
            let editor = Rc::clone(&editor);
            window.on_preview_attachment(move |index| {
                let Some(window) = weak.upgrade() else { return };
                let Ok(index) = usize::try_from(index) else { return };
                let files = files.borrow();
                let Some(file) = files.get(index) else { return };
                window.set_attachment_preview_name(file.filename.clone().into());
                window.set_attachment_preview_detail(
                    format!("{:.1} MB", file_size_mb(&file.path)).into(),
                );
                let image = image_mime(&file.path)
                    .and_then(|_| slint_image_from_path(&file.path));
                window.set_attachment_preview_has_image(image.is_some());
                if let Some(image) = image {
                    window.set_attachment_preview_image(image);
                }
                if let Some(editor) = editor.borrow().as_ref() {
                    editor.set_visible(false);
                }
                window.set_attachment_preview_open(true);
            });
        }
        {
            let weak = window.as_weak();
            let editor = Rc::clone(&editor);
            window.on_close_attachment_preview(move || {
                if let Some(window) = weak.upgrade() {
                    window.set_attachment_preview_open(false);
                }
                if let Some(editor) = editor.borrow().as_ref() {
                    editor.set_visible(true);
                }
            });
        }

        // Drag and drop (#new): the body embeds images, the rest of the window
        // attaches files, and files inside the Nextcloud folder ask whether to
        // attach or create a public link.
        let drop_pending = Rc::new(RefCell::new(None::<std::path::PathBuf>));
        {
            let weak = window.as_weak();
            let editor = Rc::clone(&editor);
            let attachment_files = Rc::clone(&attachment_files);
            let state = Rc::clone(state);
            let runtime = Rc::clone(runtime);
            let drop_pending = Rc::clone(&drop_pending);
            let cursor = Rc::new(std::cell::Cell::new((0.0f64, 0.0f64)));
            window.window().on_winit_window_event({
                let cursor = Rc::clone(&cursor);
                move |_, event| {
                    match event {
                        winit::event::WindowEvent::CursorMoved { position, .. } => {
                            cursor.set((position.x, position.y));
                        }
                        winit::event::WindowEvent::DroppedFile(path) => {
                            let path = path.clone();
                            let weak = weak.clone();
                            let editor = Rc::clone(&editor);
                            let attachment_files = Rc::clone(&attachment_files);
                            let state = Rc::clone(&state);
                            let runtime = Rc::clone(&runtime);
                            let drop_pending = Rc::clone(&drop_pending);
                            let cursor = Rc::clone(&cursor);
                            slint::Timer::single_shot(Duration::ZERO, move || {
                                let Some(window) = weak.upgrade() else { return };
                                let scale = f64::from(window.window().scale_factor()).max(0.1);
                                let (physical_x, physical_y) = cursor.get();
                                let x = physical_x / scale;
                                let y = physical_y / scale;
                                let editor_x = f64::from(window.get_editor_x());
                                let editor_y = f64::from(window.get_editor_y());
                                let editor_width = f64::from(window.get_editor_width());
                                let editor_height = f64::from(window.get_editor_height());
                                let inside_editor = x >= editor_x
                                    && x <= editor_x + editor_width
                                    && y >= editor_y
                                    && y <= editor_y + editor_height;
                                let nextcloud = window.get_nextcloud_local_path().to_string();
                                if path_inside(&nextcloud, &path) {
                                    let name = path
                                        .file_name()
                                        .map(|name| name.to_string_lossy().into_owned())
                                        .unwrap_or_default();
                                    let size = file_size_mb(&path);
                                    *drop_pending.borrow_mut() = Some(path.clone());
                                    window.set_drop_file_label(
                                        format!("{name} ({size:.1} MB)").into(),
                                    );
                                    window.set_drop_password(
                                        flectar_mail_core::nextcloud::generate_share_password()
                                            .into(),
                                    );
                                    window.set_drop_use_password(true);
                                    if let Some(editor) = editor.borrow().as_ref() {
                                        editor.set_visible(false);
                                    }
                                    window.set_drop_share_open(true);
                                    return;
                                }
                                if inside_editor
                                    && let Some(mime) = image_mime(&path)
                                {
                                    if let Ok(bytes) = std::fs::read(&path) {
                                        use base64::Engine;
                                        let encoded =
                                            base64::engine::general_purpose::STANDARD.encode(bytes);
                                        let image = format!(
                                            "<p><img src=\"data:{mime};base64,{encoded}\" style=\"max-width:100%\"></p>"
                                        );
                                        if let Some(editor) = editor.borrow().as_ref() {
                                            editor.call(
                                                "insertHtml",
                                                &[serde_json::json!(image)],
                                            );
                                        }
                                    }
                                    return;
                                }
                                let filename = path
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                attachment_files
                                    .borrow_mut()
                                    .push(ComposeAttachmentFile { path, filename });
                                let files = attachment_files.borrow();
                                apply_compose_attachments(&window, &files);
                                let _ = (&state, &runtime);
                            });
                        }
                        _ => {}
                    }
                    EventResult::Propagate
                }
            });
        }
        {
            let weak = window.as_weak();
            let drop_pending = Rc::clone(&drop_pending);
            let editor = Rc::clone(&editor);
            window.on_drop_cancel(move || {
                if let Some(editor) = editor.borrow().as_ref() {
                    editor.set_visible(true);
                }
                drop_pending.borrow_mut().take();
                if let Some(window) = weak.upgrade() {
                    window.set_drop_share_open(false);
                }
            });
        }
        {
            let weak = window.as_weak();
            let drop_pending = Rc::clone(&drop_pending);
            let attachment_files = Rc::clone(&attachment_files);
            let editor = Rc::clone(&editor);
            window.on_drop_attach(move || {
                let Some(window) = weak.upgrade() else { return };
                if let Some(editor) = editor.borrow().as_ref() {
                    editor.set_visible(true);
                }
                if let Some(path) = drop_pending.borrow_mut().take() {
                    let filename = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    attachment_files
                        .borrow_mut()
                        .push(ComposeAttachmentFile { path, filename });
                    let files = attachment_files.borrow();
                    apply_compose_attachments(&window, &files);
                }
                window.set_drop_share_open(false);
            });
        }
        {
            let weak = window.as_weak();
            let drop_pending = Rc::clone(&drop_pending);
            let editor = Rc::clone(&editor);
            let state = Rc::clone(state);
            let runtime = Rc::clone(runtime);
            {
                let weak = weak.clone();
                window.on_generate_drop_password(move || {
                    if let Some(window) = weak.upgrade() {
                        window.set_drop_password(
                            flectar_mail_core::nextcloud::generate_share_password().into(),
                        );
                    }
                });
            }
            window.on_drop_create_link(move |password, use_password, expiry_index| {
                let Some(window) = weak.upgrade() else { return };
                let Some(path) = drop_pending.borrow_mut().take() else { return };
                let Some(core) = state
                    .borrow()
                    .core
                    .as_ref()
                    .map(|core| core.account_preferences_core())
                else {
                    window.set_notice_error(true);
                    window.set_notice("Conecta una cuenta antes de insertar enlaces.".into());
                    return;
                };
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let size = file_size_mb(&path);
                let password = use_password
                    .then(|| password.to_string())
                    .filter(|value| !value.is_empty());
                let display_password = password.clone();
                let expire_days = match expiry_index {
                    0 => Some(1),
                    1 => Some(30),
                    _ => None,
                };
                if let Some(editor) = editor.borrow().as_ref() {
                    editor.set_visible(true);
                }
                window.set_drop_share_open(false);
                let task = runtime.spawn(async move {
                    core.create_nextcloud_share(
                        path.to_string_lossy().into_owned(),
                        password,
                        expire_days,
                    )
                    .await
                    .map_err(|error| error.to_string())
                });
                let weak = weak.clone();
                let editor = Rc::clone(&editor);
                let _ = slint::spawn_local(async move {
                    let result = task
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result);
                    let Some(window) = weak.upgrade() else { return };
                    match result {
                        Ok(url) => {
                            let expiry = match expire_days {
                                Some(1) => "Caducidad: 1 día",
                                Some(30) => "Caducidad: 30 días",
                                _ => "Sin caducidad",
                            };
                            let password_line = display_password
                                .as_deref()
                                .map(|password| format!("<br>Contraseña: {}", escape_html(password)))
                                .unwrap_or_default();
                            let link = format!(
                                "<p><a href=\"{}\">{}</a> ({size:.1} MB){password_line}<br>{expiry}</p>",
                                escape_html(&url),
                                escape_html(if name.is_empty() { &url } else { &name })
                            );
                            if let Some(editor) = editor.borrow().as_ref() {
                                editor.call("insertHtml", &[serde_json::json!(link)]);
                            }
                        }
                        Err(error) => {
                            window.set_notice_error(true);
                            window.set_notice(format!("Nextcloud: {error}").into());
                        }
                    }
                });
            });
        }

        editor
    };

    // Forwarding keeps the original's attachments.
    if forward
        && let Some(detail) = detail
        && !detail.attachments.is_empty()
    {
        let core = state
            .borrow()
            .core
            .as_ref()
            .map(|core| core.account_preferences_core());
        if let Some(core) = core {
            let attachments = detail
                .attachments
                .iter()
                .map(|attachment| {
                    (
                        attachment.id,
                        attachment
                            .filename
                            .clone()
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or_else(|| format!("attachment-{}", attachment.id)),
                    )
                })
                .collect::<Vec<_>>();
            let task = runtime.spawn(async move {
                let mut paths = Vec::new();
                for (id, filename) in attachments {
                    if let Ok(path) = core.get_attachment(id).await {
                        paths.push((std::path::PathBuf::from(path), filename));
                    }
                }
                paths
            });
            let weak = window.as_weak();
            let files = Rc::clone(&attachment_files);
            let _ = slint::spawn_local(async move {
                let Ok(paths) = task.await else { return };
                let Some(window) = weak.upgrade() else { return };
                let mut files = files.borrow_mut();
                for (path, filename) in paths {
                    if !files.iter().any(|file| file.path == path) {
                        files.push(ComposeAttachmentFile { path, filename });
                    }
                }
                apply_compose_attachments(&window, &files);
            });
        }
    }

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

/// New message in the separate compose window (no quoted original).
pub(super) fn compose_new(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
) -> bool {
    open_compose_window(app, state, runtime, None, "new", &compose_windows())
}

/// Reply / reply all / forward from the main window: opens the separate
/// compose window for `message_id` (or the conversation's latest message).
pub(super) fn compose_for(    app: &AppWindow,
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
                open_compose_window(&app, &state, &runtime, Some(&detail), &action, &compose_windows());
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
            let reply_mode = state
                .borrow()
                .core
                .as_ref()
                .and_then(|core| runtime.block_on(core.load_settings()).ok())
                .map(|settings| settings.reply_mode)
                .unwrap_or_else(|| app.get_reply_mode().to_string());
            if reply_mode == "tab" {
                app.invoke_select_email(row_id);
                app.invoke_compose_message_action(action);
                let _ = app.show();
                return;
            }
            compose_for(&app, &state, &runtime, row_id, None, action.as_str());
        });
    }

    let weak = app.as_weak();
    let state = Rc::clone(state);
    let runtime = Rc::clone(runtime);
    app.global::<MessageWindowApi>().on_open(move |thread_id| {
        let Some(app) = weak.upgrade() else { return };
        // A double-click on an overlay (Settings, the composer…) can reach the
        // message list underneath; only open from the list itself.
        if app.get_settings_open() || app.get_compose_open() {
            return;
        }
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
                let reply_mode = state
                    .borrow()
                    .core
                    .as_ref()
                    .and_then(|core| runtime.block_on(core.load_settings()).ok())
                    .map(|settings| settings.reply_mode)
                    .unwrap_or_else(|| app.get_reply_mode().to_string());
                if reply_mode != "tab"
                    && matches!(action.as_str(), "reply" | "reply_all" | "forward")
                    && let (Some(message), Some(detail)) =
                        (self_weak.upgrade(), loaded.borrow().as_ref())
                    && open_compose_window(&app, &state, &runtime, Some(detail), action.as_str(), &compose_windows)
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
