//! emailOO AI assistant: provider settings, user presets and the reader /
//! composer actions. The model calls go through `Core::ai_assist`.

use super::*;
use slint::{Image, SharedString};
use std::{cell::Cell, path::PathBuf, sync::Arc};

const DEFAULT_PRESETS: &str = "\
Resumir: Resume el texto en 3 a 5 puntos clave, breves y claros.
Acortar: Acorta el texto a la mitad manteniendo lo esencial y el tono.
Corregir: Corrige ortografía, gramática y puntuación sin cambiar el estilo ni el contenido.
Más formal: Reescribe el texto con un tono más formal y profesional.
Más cercano: Reescribe el texto con un tono más cercano y amable.
Responder aceptando: Escribe una respuesta breve y cordial aceptando lo que propone el mensaje.
Responder declinando: Escribe una respuesta breve y educada declinando lo que propone el mensaje.
Traducir al inglés: Traduce el texto a un inglés comercial natural y profesional.
";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Preset {
    name: String,
    instruction: String,
}

fn parse_presets(text: &str) -> Vec<Preset> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, instruction) = line.split_once(':')?;
            let (name, instruction) = (name.trim(), instruction.trim());
            (!name.is_empty() && !instruction.is_empty()).then(|| Preset {
                name: name.to_owned(),
                instruction: instruction.to_owned(),
            })
        })
        .take(50)
        .collect()
}

fn load_presets_text(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .filter(|text| !parse_presets(text).is_empty())
        .unwrap_or_else(|| DEFAULT_PRESETS.to_owned())
}

fn menu_entries(presets: &[Preset]) -> ModelRc<ActionMenuEntry> {
    let entries = presets
        .iter()
        .enumerate()
        .map(|(index, preset)| ActionMenuEntry {
            id: format!("p{index}").into(),
            title: preset.name.clone().into(),
            icon: Image::default(),
            disabled: false,
            destructive: false,
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(entries))
}

thread_local! {
    // The current presets, for the separate compose window (issue #3).
    static CURRENT_PRESETS: RefCell<Vec<Preset>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn preset_names() -> Vec<String> {
    CURRENT_PRESETS.with(|presets| presets.borrow().iter().map(|preset| preset.name.clone()).collect())
}

/// (name, instruction) of the preset at `index`.
pub(super) fn preset_at(index: usize) -> Option<(String, String)> {
    CURRENT_PRESETS.with(|presets| {
        presets
            .borrow()
            .get(index)
            .map(|preset| (preset.name.clone(), preset.instruction.clone()))
    })
}

fn preset_for(presets: &[Preset], id: &str) -> Option<Preset> {
    let index = id.strip_prefix('p')?.parse::<usize>().ok()?;
    presets.get(index).cloned()
}

fn full_core(state: &Rc<RefCell<InboxState>>) -> Option<Arc<flectar_mail_core::Core>> {
    state
        .borrow()
        .core
        .as_ref()
        .map(|core| core.account_preferences_core())
}

/// What a composer request will replace once the answer arrives.
struct ComposerTarget {
    start: usize,
    end: usize,
    original: String,
}

pub(super) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    document: &Rc<RefCell<RichComposeDocument>>,
    editor: &Rc<RefCell<LazyComposeEditor>>,
    presets_path: PathBuf,
) {
    let ui = app.global::<AiUi>();
    let presets = Rc::new(RefCell::new(Vec::<Preset>::new()));
    let generation = Rc::new(Cell::new(0u64));
    let target = Rc::new(RefCell::new(None::<ComposerTarget>));

    let apply_presets = {
        let presets = Rc::clone(&presets);
        move |app: &AppWindow, text: &str| {
            let parsed = parse_presets(text);
            let ui = app.global::<AiUi>();
            ui.set_reader_entries(menu_entries(&parsed));
            ui.set_composer_entries(menu_entries(&parsed));
            CURRENT_PRESETS.with(|current| *current.borrow_mut() = parsed.clone());
            *presets.borrow_mut() = parsed;
        }
    };
    let initial = load_presets_text(&presets_path);
    ui.set_presets_text(initial.clone().into());
    apply_presets(app, &initial);

    // Provider settings load once the core has started.
    let refresh_provider = {
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        move |app: &AppWindow| -> bool {
            let Some(core) = full_core(&state) else {
                return false;
            };
            let ui = app.global::<AiUi>();
            if let Ok(status) = runtime.block_on(core.ai_status()) {
                ui.set_configured(status.configured);
                if !status.base_url.is_empty() {
                    ui.set_base_url(status.base_url.into());
                }
                ui.set_model(status.model_cheap.into());
            }
            true
        }
    };
    {
        let weak = app.as_weak();
        let refresh = refresh_provider.clone();
        let timer = Rc::new(slint::Timer::default());
        let timer_handle = Rc::clone(&timer);
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(1500),
            move || {
                if let Some(app) = weak.upgrade()
                    && refresh(&app)
                {
                    timer_handle.stop();
                }
            },
        );
        // The timer stops itself; keep it alive until then.
        std::mem::forget(timer);
    }

    {
        let weak = app.as_weak();
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        let refresh = refresh_provider.clone();
        ui.on_save_provider(move || {
            let Some(app) = weak.upgrade() else { return };
            let ui = app.global::<AiUi>();
            let Some(core) = full_core(&state) else {
                ui.set_status_is_error(true);
                ui.set_status("emailOO todavía se está iniciando; inténtalo en un momento.".into());
                return;
            };
            let url = ui.get_base_url().trim().trim_end_matches('/').to_owned();
            let model = ui.get_model().trim().to_owned();
            let key = ui.get_api_key().trim().to_owned();
            let result = runtime.block_on(async {
                let mut settings = core.get_settings().await?;
                if !url.is_empty() {
                    settings.ai_base_url = url.clone();
                }
                if !model.is_empty() {
                    settings.ai_model_instant = model.clone();
                    settings.ai_model_cheap = model.clone();
                    settings.ai_model_intelligent = model.clone();
                }
                core.set_settings(settings).await?;
                if !key.is_empty() {
                    core.set_ai_key(key).await?;
                }
                Ok::<(), flectar_mail_core::error::CoreError>(())
            });
            match result {
                Ok(()) => {
                    ui.set_api_key(SharedString::new());
                    refresh(&app);
                    ui.set_status_is_error(false);
                    ui.set_status(if ui.get_configured() {
                        "Ajustes de IA guardados.".into()
                    } else {
                        "Guardado. Añade una clave API o usa un servicio local http://.".into()
                    });
                }
                Err(error) => {
                    ui.set_status_is_error(true);
                    ui.set_status(format!("No se pudo guardar: {error}").into());
                }
            }
        });
    }

    {
        let weak = app.as_weak();
        let path = presets_path.clone();
        let apply = apply_presets.clone();
        ui.on_save_presets(move || {
            let Some(app) = weak.upgrade() else { return };
            let ui = app.global::<AiUi>();
            let text = ui.get_presets_text().to_string();
            if parse_presets(&text).is_empty() {
                ui.set_status_is_error(true);
                ui.set_status("Añade al menos un preset como «Nombre: instrucción».".into());
                return;
            }
            match std::fs::write(&path, &text) {
                Ok(()) => {
                    apply(&app, &text);
                    ui.set_status_is_error(false);
                    ui.set_status("Presets guardados.".into());
                }
                Err(error) => {
                    ui.set_status_is_error(true);
                    ui.set_status(format!("No se pudieron guardar los presets: {error}").into());
                }
            }
        });
    }

    {
        let weak = app.as_weak();
        let path = presets_path.clone();
        let apply = apply_presets.clone();
        ui.on_reset_presets(move || {
            let Some(app) = weak.upgrade() else { return };
            let _ = std::fs::remove_file(&path);
            app.global::<AiUi>().set_presets_text(DEFAULT_PRESETS.into());
            apply(&app, DEFAULT_PRESETS);
        });
    }

    // Runs one request and shows the answer in the result panel.
    let run = {
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        let generation = Rc::clone(&generation);
        move |app: &AppWindow, title: String, instruction: String, text: String, for_composer: bool| {
            let ui = app.global::<AiUi>();
            let Some(core) = full_core(&state) else {
                ui.set_result_title("AI".into());
                ui.set_result("emailOO todavía se está iniciando; inténtalo en un momento.".into());
                ui.set_result_for_composer(false);
                ui.set_result_open(true);
                return;
            };
            generation.set(generation.get().wrapping_add(1));
            let expected = generation.get();
            ui.set_result_title(title.clone().into());
            ui.set_result(SharedString::new());
            ui.set_result_for_composer(for_composer);
            ui.set_busy(true);
            if for_composer {
                app.set_compose_notice(UiMessage::detail("AI · {}…", title.clone()));
                app.set_compose_notice_is_error(false);
            }
            let task = runtime.spawn(async move { core.ai_assist(instruction, text).await });
            let abort = task.abort_handle();
            let weak = app.as_weak();
            let generation = Rc::clone(&generation);
            if let Err(error) = slint::spawn_local(async move {
                let result = task
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map_err(|error| error.to_string()));
                if generation.get() != expected {
                    return;
                }
                let Some(app) = weak.upgrade() else { return };
                let ui = app.global::<AiUi>();
                ui.set_busy(false);
                match (result, for_composer) {
                    (Ok(answer), true) => {
                        ui.set_result(answer.into());
                        ui.invoke_apply_result_to_composer();
                    }
                    (Ok(answer), false) => {
                        ui.set_result(answer.into());
                        ui.set_result_open(true);
                    }
                    (Err(error), true) => {
                        app.set_compose_notice(UiMessage::detail("AI: {}", error));
                        app.set_compose_notice_is_error(true);
                    }
                    (Err(error), false) => {
                        ui.set_result_for_composer(false);
                        ui.set_result_title(format!("{title} · error").into());
                        ui.set_result(error.into());
                        ui.set_result_open(true);
                    }
                }
            }) {
                abort.abort();
                ui.set_busy(false);
                ui.set_result(error.to_string().into());
            }
        }
    };

    {
        let weak = app.as_weak();
        let presets = Rc::clone(&presets);
        let run = run.clone();
        ui.on_run_on_reader(move |id, text| {
            let Some(app) = weak.upgrade() else { return };
            // While a draft is open, the AI menu works on the draft: the
            // selection there is replaced in place.
            if app.get_compose_open() {
                app.global::<AiUi>().invoke_run_on_composer(id);
                return;
            }
            let Some(preset) = preset_for(&presets.borrow(), id.as_str()) else { return };
            run(&app, preset.name, preset.instruction, text.to_string(), false);
        });
    }

    {
        let weak = app.as_weak();
        let presets = Rc::clone(&presets);
        let document = Rc::clone(document);
        let target = Rc::clone(&target);
        let run = run.clone();
        ui.on_run_on_composer(move |id| {
            let Some(app) = weak.upgrade() else { return };
            let Some(preset) = preset_for(&presets.borrow(), id.as_str()) else { return };
            let document = document.borrow();
            let text = document.text().to_owned();
            let selection = document.selection();
            let (a, b) = (
                usize::try_from(selection.start.max(0)).unwrap_or(0).min(text.len()),
                usize::try_from(selection.end.max(0)).unwrap_or(0).min(text.len()),
            );
            let (mut start, mut end) = (a.min(b), a.max(b));
            if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                start = 0;
                end = 0;
            }
            if start == end {
                // No selection: the whole draft, or new text for an empty one.
                start = 0;
                end = text.len();
            }
            let input = text[start..end].to_owned();
            *target.borrow_mut() = Some(ComposerTarget {
                start,
                end,
                original: text,
            });
            drop(document);
            run(&app, preset.name, preset.instruction, input, true);
        });
    }

    {
        let weak = app.as_weak();
        let document = Rc::clone(document);
        let editor = Rc::clone(editor);
        let target = Rc::clone(&target);
        ui.on_apply_result_to_composer(move || {
            let Some(app) = weak.upgrade() else { return };
            let ui = app.global::<AiUi>();
            let Some(target) = target.borrow_mut().take() else { return };
            let answer = ui.get_result().to_string();
            let mut document = document.borrow_mut();
            if document.text() != target.original || !app.get_compose_open() {
                app.set_compose_notice(UiMessage::plain(
                    "The draft changed while AI was working; copy the result instead.",
                ));
                app.set_compose_notice_is_error(true);
                return;
            }
            document.update_selection(target.start as i32, target.end as i32);
            let selection = document.insert_text(&answer);
            apply_rich_compose(&app, &document, selection, &mut editor.borrow_mut());
            ui.set_result_open(false);
            app.set_compose_notice(UiMessage::plain("AI text applied. Undo with Cmd+Z."));
            app.set_compose_notice_is_error(false);
        });
    }

    {
        let generation = Rc::clone(&generation);
        let weak = app.as_weak();
        ui.on_cancel(move || {
            generation.set(generation.get().wrapping_add(1));
            if let Some(app) = weak.upgrade() {
                app.global::<AiUi>().set_busy(false);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_parse_name_and_instruction_lines() {
        let presets = parse_presets("# comment\nShort: Make it short\n\nbad line\nFormal:  Be formal: always \n");
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[1].name, "Formal");
        assert_eq!(presets[1].instruction, "Be formal: always");
        assert_eq!(preset_for(&presets, "p1"), Some(presets[1].clone()));
        assert_eq!(preset_for(&presets, "p9"), None);
        assert!(!parse_presets(DEFAULT_PRESETS).is_empty());
    }
}
