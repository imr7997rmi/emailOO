//! emailOO: keep the portable settings backup in iCloud Drive so other Macs
//! pick it up (same idea as fileOO). The choice is stored per Mac in
//! `icloud-sync.json`; the shared file is `iCloud Drive/emailOO/config.json`
//! and never carries credentials.

use super::*;
use slint::SharedString;
use crate::data_controller::{build_account_backup, restore_account_backup};
use std::{
    cell::Cell,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::SystemTime,
};

const TICK: std::time::Duration = std::time::Duration::from_secs(20);

fn icloud_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let drive = home.join("Library/Mobile Documents/com~apple~CloudDocs");
    drive.is_dir().then(|| drive.join("emailOO"))
}

fn remote_path() -> Option<PathBuf> {
    icloud_dir().map(|dir| dir.join("config.json"))
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Marker {
    #[serde(default)]
    icloud: bool,
}

fn enabled(marker: &Path) -> bool {
    std::fs::read(marker)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok())
        .is_some_and(|marker| marker.icloud)
        && icloud_dir().is_some()
}

/// Hash of the backup without its timestamp, so unchanged settings are not
/// rewritten. `serde_json` objects are ordered maps, so this is stable.
fn content_hash(backup: &serde_json::Value) -> u64 {
    let mut value = backup.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("createdAt");
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    hasher.finish()
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

struct SyncState {
    marker: PathBuf,
    last_hash: Cell<u64>,
    last_remote: Cell<Option<SystemTime>>,
}

fn now_label() -> String {
    Local::now().format("%H:%M").to_string()
}

/// Pull a newer remote file, then push local changes.
fn tick(
    app: &AppWindow,
    sync: &SyncState,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
) {
    if !enabled(&sync.marker) {
        return;
    }
    let ui = app.global::<CloudSync>();
    let Some(remote) = remote_path() else { return };
    let (core, configs) = {
        let state = state.borrow();
        (state.core.clone(), state.account_configs.clone())
    };
    let Some(core) = core else { return };

    let remote_time = modified(&remote);
    if remote_time.is_some() && remote_time != sync.last_remote.get() {
        sync.last_remote.set(remote_time);
        if let Ok(bytes) = std::fs::read(&remote)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            let hash = content_hash(&value);
            if hash != sync.last_hash.get() {
                match restore_account_backup(app, &bytes, &core, state, runtime) {
                    Ok(_) => {
                        sync.last_hash.set(hash);
                        ui.set_status(format!("Recibido de iCloud a las {}.", now_label()).into());
                    }
                    Err(error) => {
                        ui.set_status(format!("No se pudo aplicar la copia de iCloud: {error}").into());
                    }
                }
                return;
            }
        }
    }

    if configs.is_empty() {
        return;
    }
    let backup = build_account_backup(Some(&core), &configs, runtime);
    let hash = content_hash(&backup);
    if hash == sync.last_hash.get() {
        return;
    }
    let Some(dir) = icloud_dir() else { return };
    let written = std::fs::create_dir_all(&dir).and_then(|()| {
        let bytes = serde_json::to_vec_pretty(&backup).map_err(std::io::Error::other)?;
        let staging = dir.join(".config.json.tmp");
        std::fs::write(&staging, bytes)?;
        std::fs::rename(&staging, &remote)
    });
    match written {
        Ok(()) => {
            sync.last_hash.set(hash);
            sync.last_remote.set(modified(&remote));
            ui.set_status(format!("Sincronizado con iCloud a las {}.", now_label()).into());
        }
        Err(error) => ui.set_status(format!("No se pudo escribir en iCloud: {error}").into()),
    }
}

pub(super) fn register(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &Rc<tokio::runtime::Runtime>,
    data_dir: PathBuf,
) {
    let ui = app.global::<CloudSync>();
    let sync = Rc::new(SyncState {
        marker: data_dir.join("icloud-sync.json"),
        last_hash: Cell::new(0),
        last_remote: Cell::new(None),
    });
    ui.set_available(icloud_dir().is_some());
    ui.set_enabled(enabled(&sync.marker));

    {
        let weak = app.as_weak();
        let sync = Rc::clone(&sync);
        let state = Rc::clone(state);
        let runtime = Rc::clone(runtime);
        ui.on_toggle(move |on| {
            let Some(app) = weak.upgrade() else { return };
            let ui = app.global::<CloudSync>();
            let bytes = serde_json::to_vec(&Marker { icloud: on }).unwrap_or_default();
            if let Err(error) = std::fs::write(&sync.marker, bytes) {
                ui.set_enabled(!on);
                ui.set_status(format!("No se pudo guardar la preferencia: {error}").into());
                return;
            }
            ui.set_enabled(on);
            ui.set_status(SharedString::new());
            // Start from the remote copy when it exists, otherwise publish ours.
            sync.last_hash.set(0);
            sync.last_remote.set(None);
            if on {
                tick(&app, &sync, &state, &runtime);
            }
        });
    }

    let weak = app.as_weak();
    let state = Rc::clone(state);
    let runtime = Rc::clone(runtime);
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, TICK, move || {
        if let Some(app) = weak.upgrade() {
            tick(&app, &sync, &state, &runtime);
        }
    });
    // Runs for the life of the app.
    std::mem::forget(timer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_ignores_the_timestamp() {
        let a = serde_json::json!({ "createdAt": "1", "accounts": [1] });
        let b = serde_json::json!({ "createdAt": "2", "accounts": [1] });
        let c = serde_json::json!({ "createdAt": "2", "accounts": [2] });
        assert_eq!(content_hash(&a), content_hash(&b));
        assert_ne!(content_hash(&a), content_hash(&c));
    }
}
