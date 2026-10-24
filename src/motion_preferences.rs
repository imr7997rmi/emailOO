//! The "Reduce motion" preference. It is read before the first frame so the
//! window never animates when the user turned animations off.
use crate::{AppWindow, MotionSettings};
use slint::ComponentHandle;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(serde::Serialize, serde::Deserialize)]
struct Preference {
    #[serde(default = "enabled_by_default")]
    animations: bool,
}

fn enabled_by_default() -> bool {
    true
}

fn load(path: &Path) -> bool {
    let result = (|| -> Result<Preference, Box<dyn std::error::Error>> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?.take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err("motion preference is too large".into());
        }
        Ok(serde_json::from_slice(&bytes)?)
    })();
    match result {
        Ok(preference) => preference.animations,
        Err(error) => {
            if path.exists() {
                eprintln!("motion preference unavailable; animations stay on: {error}");
            }
            true
        }
    }
}

fn save(path: &Path, animations: bool) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().ok_or("motion preference has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, &Preference { animations })?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub(crate) fn register(app: &AppWindow, path: PathBuf) {
    let motion = app.global::<MotionSettings>();
    motion.set_enabled(load(&path));
    motion.on_save(move |animations| {
        if let Err(error) = save(&path, animations) {
            tracing::warn!(%error, "could not save the motion preference");
        }
    });
}
