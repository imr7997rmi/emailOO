//! emailOO: create Nextcloud public share links (#4).
//!
//! The desktop client's `nextcloud.cfg` tells us which local folder maps to
//! which remote path and which server/user it belongs to. The app password is
//! stored in the keychain (account id 0, password slot) and never exported.

use crate::error::{CoreError, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct NextcloudConfig {
    pub url: String,
    pub username: String,
    pub password: String,
    pub local_path: String,
    pub remote_path: String,
}

fn nextcloud_cfg_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(
            home.join("Library/Containers/com.nextcloud.desktopclient/Data/Library/Preferences/Nextcloud/nextcloud.cfg"),
        );
        paths.push(home.join(".config/Nextcloud/nextcloud.cfg"));
        paths.push(home.join("Library/Preferences/Nextcloud/nextcloud.cfg"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        paths.push(PathBuf::from(appdata).join("Nextcloud/nextcloud.cfg"));
    }
    paths
}

/// First folder mapping found in the desktop client's configuration.
pub fn detect_folder() -> Option<(String, String)> {
    for path in nextcloud_cfg_paths() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut local = None;
        let mut target = None;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if key.ends_with("localPath") && local.is_none() && !value.is_empty() {
                local = Some(value.to_owned());
            }
            if key.ends_with("targetPath") && target.is_none() {
                target = Some(value.to_owned());
            }
            if local.is_some() && target.is_some() {
                break;
            }
        }
        if let Some(local) = local {
            let target = target.unwrap_or_else(|| "/".to_owned());
            return Some((local, target));
        }
    }
    None
}

fn join_remote(base: &str, relative: &Path) -> String {
    let mut remote = base.trim_end_matches('/').to_owned();
    for component in relative.components() {
        let Some(part) = component.as_os_str().to_str() else {
            continue;
        };
        if part.is_empty() {
            continue;
        }
        remote.push('/');
        remote.push_str(part);
    }
    if remote.is_empty() {
        "/".to_owned()
    } else {
        remote
    }
}

fn xml_value(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim().to_owned())
}

/// Create (or reuse) a public link for `file`, which must live inside the
/// configured Nextcloud folder. Returns the public URL.
/// Random password for public links (#4). 20 characters from a
/// no-ambiguity alphabet; enough for a one-off file share.
pub fn generate_share_password() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rng();
    (0..20)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Optional protections for a public share (#4).
#[derive(Debug, Clone, Default)]
pub struct ShareOptions {
    /// Password required to open the link. `None` leaves it open.
    pub password: Option<String>,
    /// Days until the link expires; `None` never expires.
    pub expire_days: Option<u32>,
}

pub async fn create_public_share(
    config: &NextcloudConfig,
    file: &Path,
    options: &ShareOptions,
) -> Result<String> {
    let base = Path::new(&config.local_path);
    let relative = file.strip_prefix(base).map_err(|_| {
        CoreError::Other("the file is not inside the configured Nextcloud folder".into())
    })?;
    let remote = join_remote(&config.remote_path, relative);
    let endpoint = format!(
        "{}/ocs/v2.php/apps/files_sharing/api/v1/shares",
        config.url.trim_end_matches('/')
    );
    let client = reqwest::Client::new();
    let mut form = vec![
        ("path".to_owned(), remote),
        ("shareType".to_owned(), "3".to_owned()),
        ("permissions".to_owned(), "1".to_owned()),
    ];
    if let Some(password) = options.password.as_deref().filter(|value| !value.is_empty()) {
        form.push(("password".to_owned(), password.to_owned()));
    }
    if let Some(days) = options.expire_days {
        let date = chrono::Local::now().date_naive() + chrono::Duration::days(i64::from(days));
        form.push(("expireDate".to_owned(), date.format("%Y-%m-%d").to_string()));
    }
    let response = client
        .post(endpoint)
        .basic_auth(&config.username, Some(&config.password))
        .header("OCS-APIRequest", "true")
        .header("Accept", "application/xml")
        .form(&form)
        .send()
        .await
        .map_err(|error| CoreError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| CoreError::Network(error.to_string()))?;
    if let Some(url) = xml_value(&response, "url") {
        return Ok(url);
    }
    if let Some(token) = xml_value(&response, "token") {
        return Ok(format!(
            "{}/s/{token}",
            config.url.trim_end_matches('/')
        ));
    }
    Err(CoreError::Other(format!(
        "Nextcloud did not return a share link: {}",
        response.chars().take(300).collect::<String>()
    )))
}
