//! Comprobación y descarga de actualizaciones desde el feed de Nivvoo Apps.
//!
//! Reutiliza el protocolo de fileOO: feed JSON pequeño, descarga con `curl`,
//! validación SHA-256 con `shasum`, extracción con `ditto` y un script que
//! sustituye la app cuando termina el proceso. La UI es Slint.

use crate::AppWindow;
use serde::Deserialize;
use slint::{ComponentHandle, Weak};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEFAULT_FEED: &str =
    "https://raw.githubusercontent.com/imr7997rmi/nivvoo-apps/main/emailOO-latest.json";
const APP_NAME: &str = "emailOO";

#[derive(Clone, Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    version: String,
    #[serde(default)]
    build: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    notes: String,
}

#[derive(Default)]
struct UpdateState {
    generation: u64,
    checking: bool,
    downloading: bool,
    cancel: bool,
    manifest: Option<Manifest>,
    child: Option<Child>,
}

fn feed_url() -> String {
    std::env::var("EMAILOO_UPDATE_URL").unwrap_or_else(|_| DEFAULT_FEED.to_string())
}

fn resolve_url(manifest: &Manifest, feed: &str) -> String {
    if manifest.url.starts_with("http://") || manifest.url.starts_with("https://") {
        return manifest.url.clone();
    }
    let name = if !manifest.file.is_empty() {
        manifest.file.clone()
    } else if !manifest.url.is_empty() {
        manifest.url.clone()
    } else {
        format!("{APP_NAME}-latest.zip")
    };
    let base = feed.rsplit_once('/').map(|(base, _)| base).unwrap_or(feed);
    format!("{base}/{name}")
}

pub(crate) fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(target_os = "macos")]
pub(crate) fn current_build() -> String {
    use objc2::MainThreadMarker;
    use objc2_foundation::{NSBundle, NSString};

    if MainThreadMarker::new().is_none() {
        return String::new();
    }
    let key = NSString::from_str("CFBundleVersion");
    NSBundle::mainBundle()
        .objectForInfoDictionaryKey(&key)
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|value| value.to_string())
        .unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn current_build() -> String {
    String::new()
}

fn set_ui<F>(weak: &Weak<AppWindow>, update: F)
where
    F: FnOnce(&AppWindow) + Send + 'static,
{
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            update(&app);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn show_dialog(
    weak: &Weak<AppWindow>,
    title: &str,
    message: &str,
    notes: &str,
    busy: bool,
    available: bool,
    error: bool,
    progress: f32,
    progress_text: &str,
) {
    let title = title.to_string();
    let message = message.to_string();
    let notes = notes.to_string();
    let progress_text = progress_text.to_string();
    set_ui(weak, move |app| {
        app.set_update_dialog_title(title.into());
        app.set_update_dialog_message(message.into());
        app.set_update_dialog_notes(notes.into());
        app.set_update_dialog_busy(busy);
        app.set_update_dialog_available(available);
        app.set_update_dialog_error(error);
        app.set_update_dialog_progress(progress);
        app.set_update_dialog_progress_text(progress_text.into());
        app.set_update_dialog_open(true);
    });
}

fn close_dialog(weak: &Weak<AppWindow>) {
    set_ui(weak, |app| app.set_update_dialog_open(false));
}

fn set_progress(weak: &Weak<AppWindow>, progress: f32, text: String) {
    set_ui(weak, move |app| {
        app.set_update_dialog_progress(progress);
        app.set_update_dialog_progress_text(text.into());
    });
}

fn fail(weak: &Weak<AppWindow>, message: &str, workdir: Option<&Path>) {
    if let Some(workdir) = workdir {
        let _ = std::fs::remove_dir_all(workdir);
    }
    show_dialog(
        weak,
        "No se pudo actualizar",
        message,
        "",
        false,
        false,
        true,
        0.0,
        "",
    );
}

fn fetch_manifest() -> Result<Manifest, String> {
    let url = feed_url();
    let output = Command::new("/usr/bin/curl")
        .args(["-fL", "--max-time", "20", "--silent", "--show-error", &url])
        .output()
        .map_err(|error| format!("No se pudo ejecutar curl: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("Error HTTP al consultar {url}")
        } else {
            detail
        });
    }
    let body = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&body).map_err(|error| format!("El feed no es válido: {error}"))
}

fn check_updates(weak: &Weak<AppWindow>, state: &Arc<Mutex<UpdateState>>) {
    let generation = {
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        if state.checking || state.downloading {
            return;
        }
        state.checking = true;
        state.cancel = false;
        state.generation = state.generation.wrapping_add(1);
        state.generation
    };
    show_dialog(
        weak,
        "Actualización",
        "Comprobando actualizaciones…",
        "",
        true,
        false,
        false,
        0.0,
        "",
    );
    let weak = weak.clone();
    let state = state.clone();
    std::thread::spawn(move || {
        let result = fetch_manifest();
        let mut locked = state.lock().unwrap_or_else(|error| error.into_inner());
        if locked.generation != generation {
            return;
        }
        locked.checking = false;
        if locked.cancel {
            locked.cancel = false;
            drop(locked);
            close_dialog(&weak);
            return;
        }
        let build = current_build();
        let version = current_version();
        match result {
            Ok(manifest) => {
                let newer = if !manifest.build.is_empty() && !build.is_empty() {
                    manifest.build > build
                } else {
                    !manifest.version.is_empty() && manifest.version != version
                };
                if !newer {
                    locked.manifest = None;
                    drop(locked);
                    let mut message = format!("{APP_NAME} {version}");
                    if !build.is_empty() {
                        message.push_str(&format!(" (compilación {build})"));
                    }
                    show_dialog(
                        &weak,
                        "Estás en la última versión",
                        &message,
                        "",
                        false,
                        false,
                        false,
                        0.0,
                        "",
                    );
                    return;
                }
                let mut message = format!("{APP_NAME} {} ({})", manifest.version, manifest.build);
                if manifest.size > 0 {
                    message.push_str(&format!(" · {}", format_bytes(manifest.size)));
                }
                let notes = manifest.notes.trim().to_string();
                locked.manifest = Some(manifest);
                drop(locked);
                show_dialog(
                    &weak,
                    "Nueva versión disponible",
                    &message,
                    &notes,
                    false,
                    true,
                    false,
                    0.0,
                    "",
                );
            }
            Err(error) => {
                locked.manifest = None;
                drop(locked);
                show_dialog(
                    &weak,
                    "No se pudo comprobar la actualización",
                    &error,
                    "",
                    false,
                    false,
                    true,
                    0.0,
                    "",
                );
            }
        }
    });
}

fn start_update(weak: &Weak<AppWindow>, state: &Arc<Mutex<UpdateState>>) {
    let manifest = {
        let mut locked = state.lock().unwrap_or_else(|error| error.into_inner());
        if locked.downloading {
            return;
        }
        let Some(manifest) = locked.manifest.clone() else {
            return;
        };
        locked.downloading = true;
        locked.cancel = false;
        locked.generation = locked.generation.wrapping_add(1);
        manifest
    };
    show_dialog(
        weak,
        "Actualización",
        &format!("Descargando {APP_NAME} {}…", manifest.version),
        manifest.notes.trim(),
        true,
        false,
        false,
        0.0,
        "",
    );
    let weak = weak.clone();
    let state = state.clone();
    std::thread::spawn(move || download_and_install(weak, state, manifest));
}

fn cancel_update(weak: &Weak<AppWindow>, state: &Arc<Mutex<UpdateState>>) {
    {
        let mut locked = state.lock().unwrap_or_else(|error| error.into_inner());
        locked.cancel = true;
        locked.generation = locked.generation.wrapping_add(1);
        locked.checking = false;
        locked.downloading = false;
        if let Some(child) = locked.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        locked.child = None;
    }
    close_dialog(weak);
}

fn download_and_install(
    weak: Weak<AppWindow>,
    state: Arc<Mutex<UpdateState>>,
    manifest: Manifest,
) {
    let workdir = std::env::temp_dir().join(format!("{APP_NAME}-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    if let Err(error) = std::fs::create_dir_all(&workdir) {
        fail(&weak, &format!("No se pudo crear la carpeta temporal: {error}"), None);
        return;
    }
    let zip = workdir.join(format!("{APP_NAME}-latest.zip"));
    let url = resolve_url(&manifest, &feed_url());
    let child = match Command::new("/usr/bin/curl")
        .args(["-fL", "--silent", "--show-error", "--output"])
        .arg(&zip)
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            fail(&weak, &format!("No se pudo iniciar la descarga: {error}"), Some(&workdir));
            return;
        }
    };
    {
        let mut locked = state.lock().unwrap_or_else(|error| error.into_inner());
        locked.child = Some(child);
    }

    let mut finished = false;
    let mut failed = false;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let size = std::fs::metadata(&zip).map(|meta| meta.len()).unwrap_or(0);
        let mut locked = state.lock().unwrap_or_else(|error| error.into_inner());
        if locked.cancel {
            if let Some(child) = locked.child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            locked.child = None;
            locked.downloading = false;
            drop(locked);
            let _ = std::fs::remove_dir_all(&workdir);
            return;
        }
        match locked.child.as_mut().map(|child| child.try_wait()) {
            Some(Ok(Some(_))) => {
                finished = true;
            }
            Some(Err(_)) | None => {
                failed = true;
            }
            _ => {}
        }
        drop(locked);
        if manifest.size > 0 {
            let progress = (size as f64 / manifest.size as f64).clamp(0.0, 1.0) as f32;
            set_progress(
                &weak,
                progress,
                format!("{} de {}", format_bytes(size), format_bytes(manifest.size)),
            );
        } else {
            set_progress(&weak, 0.0, format_bytes(size));
        }
        if finished || failed {
            break;
        }
    }

    let mut locked = state.lock().unwrap_or_else(|error| error.into_inner());
    let status = locked
        .child
        .take()
        .and_then(|mut child| child.wait().ok())
        .map(|status| status.success())
        .unwrap_or(false);
    locked.downloading = false;
    drop(locked);
    if !status || failed {
        fail(&weak, "La descarga no terminó correctamente.", Some(&workdir));
        return;
    }

    set_progress(&weak, 1.0, "Verificando la descarga…".to_string());
    let hash = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(&zip)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_lowercase()
        })
        .unwrap_or_default();
    if !manifest.sha256.is_empty() && hash != manifest.sha256.to_lowercase() {
        fail(
            &weak,
            "El archivo descargado no coincide con el SHA-256 publicado.",
            Some(&workdir),
        );
        return;
    }

    set_progress(&weak, 1.0, "Descomprimiendo la actualización…".to_string());
    let extract = workdir.join("extraido");
    let _ = std::fs::create_dir_all(&extract);
    let unzip = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(&zip)
        .arg(&extract)
        .status();
    if !unzip.map(|status| status.success()).unwrap_or(false) {
        fail(&weak, "No se pudo descomprimir la actualización.", Some(&workdir));
        return;
    }
    let app = std::fs::read_dir(&extract)
        .ok()
        .and_then(|entries| {
            entries.flatten().map(|entry| entry.path()).find(|path| {
                path.extension()
                    .map(|extension| extension == "app")
                    .unwrap_or(false)
            })
        });
    let Some(app) = app else {
        fail(&weak, "El ZIP no contiene ninguna aplicación.", Some(&workdir));
        return;
    };
    install_update(&weak, &app, &workdir);
}

fn install_update(weak: &Weak<AppWindow>, new_app: &Path, workdir: &Path) {
    let Some(target) = target_app() else {
        fail(
            weak,
            "No se pudo localizar la aplicación instalada.",
            Some(workdir),
        );
        return;
    };
    let script = workdir.join("instalar.sh");
    let body = format!(
        "#!/bin/sh\npid=$1\ntarget=$2\nnew=$3\ntmp=$4\nwhile kill -0 \"$pid\" 2>/dev/null; do sleep 0.2; done\nbackup=\"$target.old.$$\"\nrm -rf \"$backup\"\nif mv \"$target\" \"$backup\" 2>/dev/null; then\n  if ditto \"$new\" \"$target\"; then\n    xattr -dr com.apple.quarantine \"$target\" 2>/dev/null || true\n    rm -rf \"$backup\"\n    open \"$target\"\n  else\n    rm -rf \"$target\"\n    mv \"$backup\" \"$target\"\n    open \"$target\"\n  fi\nelse\n  open \"$target\"\nfi\nrm -rf \"$tmp\"\n"
    );
    if let Err(error) = std::fs::write(&script, body) {
        fail(
            weak,
            &format!("No se pudo preparar el instalador: {error}"),
            Some(workdir),
        );
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/bin/sh");
        command
            .arg(&script)
            .arg(std::process::id().to_string())
            .arg(&target)
            .arg(new_app)
            .arg(workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        if command.spawn().is_err() {
            fail(
                weak,
                "No se pudo iniciar el instalador.",
                Some(workdir),
            );
            return;
        }
    }
    #[cfg(not(unix))]
    {
        fail(
            weak,
            "La instalación automática solo está disponible en macOS.",
            Some(workdir),
        );
        return;
    }
    show_dialog(
        weak,
        "Actualización lista",
        "La aplicación se reiniciará para instalar la nueva versión.",
        "",
        false,
        false,
        false,
        1.0,
        "Instalando…",
    );
    std::process::exit(0);
}

fn target_app() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    executable
        .ancestors()
        .find(|path| path.extension().map(|extension| extension == "app").unwrap_or(false))
        .map(Path::to_path_buf)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(target_os = "macos")]
mod macos_menu {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSApplication, NSMenuItem};
    use objc2_foundation::NSString;
    use std::cell::{OnceCell, RefCell};

    thread_local! {
        static CALLBACK: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
        static TARGET: OnceCell<Retained<UpdateMenuTarget>> = const { OnceCell::new() };
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "EmailOOUpdateMenuTarget"]
        struct UpdateMenuTarget;

        impl UpdateMenuTarget {
            #[unsafe(method(checkForUpdates:))]
            fn check_for_updates(&self, _sender: Option<&AnyObject>) {
                CALLBACK.with(|callback| {
                    if let Some(callback) = callback.borrow().as_ref() {
                        callback();
                    }
                });
            }
        }
    );

    pub(super) fn install(callback: impl Fn() + 'static) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        CALLBACK.with(|slot| *slot.borrow_mut() = Some(Box::new(callback)));
        let target = TARGET.with(|slot| {
            slot.get_or_init(|| unsafe {
                msg_send![UpdateMenuTarget::alloc(mtm), init]
            })
            .clone()
        });
        unsafe {
            let app = NSApplication::sharedApplication(mtm);
            let Some(main) = app.mainMenu() else {
                return;
            };
            let mut application_menu = None;
            for index in 0..main.numberOfItems() {
                if let Some(item) = main.itemAtIndex(index)
                    && let Some(submenu) = item.submenu()
                    && submenu.itemWithTitle(&NSString::from_str("About emailOO")).is_some()
                {
                    application_menu = Some(submenu);
                    break;
                }
            }
            let application_menu = application_menu
                .or_else(|| main.itemAtIndex(1).and_then(|item| item.submenu()));
            let Some(application_menu) = application_menu else {
                return;
            };
            let title = NSString::from_str("Buscar actualizaciones…");
            if application_menu.itemWithTitle(&title).is_some() {
                return;
            }
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &title,
                Some(sel!(checkForUpdates:)),
                &NSString::from_str(""),
            );
            item.setTarget(Some(&target));
            let index = application_menu.numberOfItems().min(1);
            application_menu.insertItem_atIndex(&item, index);
        }
    }
}

pub(crate) fn register(app: &AppWindow) {
    app.set_app_build(current_build().into());
    let weak = app.as_weak();
    let state = Arc::new(Mutex::new(UpdateState::default()));

    app.on_check_updates({
        let weak = weak.clone();
        let state = state.clone();
        move || check_updates(&weak, &state)
    });
    app.on_update_install({
        let weak = weak.clone();
        let state = state.clone();
        move || start_update(&weak, &state)
    });
    app.on_update_cancel({
        let weak = weak.clone();
        let state = state.clone();
        move || cancel_update(&weak, &state)
    });
    app.on_update_dialog_close({
        let weak = weak.clone();
        move || close_dialog(&weak)
    });

    #[cfg(target_os = "macos")]
    {
        let weak = weak.clone();
        // AppKit installs its default application menu while finishing launch;
        // adding the item once the event loop is running keeps it in that menu.
        slint::Timer::single_shot(Duration::from_secs(1), move || {
            let weak = weak.clone();
            macos_menu::install(move || {
                if let Some(app) = weak.upgrade() {
                    app.invoke_check_updates();
                }
            });
        });
    }

    // Headless verification hook: EMAILOO_AUTO_CHECK_UPDATES=1 opens the same
    // dialog as the menu item shortly after startup.
    if std::env::var("EMAILOO_AUTO_CHECK_UPDATES").as_deref() == Ok("1") {
        let weak = weak.clone();
        slint::Timer::single_shot(Duration::from_secs(2), move || {
            if let Some(app) = weak.upgrade() {
                app.invoke_check_updates();
            }
        });
    }
}
