use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};
use url::Url;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub public_url: Url,
    pub auth_token_file: PathBuf,
    pub comet: CometConfig,
    #[serde(default = "max_upload")]
    pub max_upload_bytes: u64,
    #[serde(default = "transfer_timeout")]
    pub transfer_timeout_seconds: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CometConfig {
    pub url: Url,
    #[serde(default = "default_user")]
    pub username: String,
    pub password_file: PathBuf,
    /// Optional PEM CA for a private certificate. TLS verification is never disabled.
    pub ca_file: Option<PathBuf>,
    #[serde(default = "ports")]
    pub ports: u8,
}
fn max_upload() -> u64 {
    64 * 1024 * 1024 * 1024
}
fn transfer_timeout() -> u64 {
    7200
}
fn ports() -> u8 {
    4
}
fn default_user() -> String {
    "admin".into()
}

impl Config {
    pub fn read(path: &Path) -> std::result::Result<Self, &'static str> {
        let text = std::fs::read_to_string(path).map_err(|_| "cannot read configuration file")?;
        let config: Self = toml::from_str(&text).map_err(|_| "invalid configuration TOML")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        for url in [&self.public_url, &self.comet.url] {
            if url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.path() != "/"
            {
                return Err(
                    "URLs must be origins without paths, embedded credentials, query or fragment",
                );
            }
            let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
            if url.scheme() != "https" && !(url.scheme() == "http" && local) {
                return Err("HTTPS required except for explicit loopback HTTP");
            }
        }
        if !(1..=4).contains(&self.comet.ports)
            || self.max_upload_bytes == 0
            || !(60..=86400).contains(&self.transfer_timeout_seconds)
        {
            return Err("invalid port count, upload bound or transfer timeout");
        }
        if !self.listen.ip().is_loopback() {
            return Err("listen must be loopback; use a private HTTPS proxy");
        }
        Ok(())
    }
}

pub fn read_secret(path: &Path) -> std::result::Result<String, &'static str> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let file = std::fs::File::open(path).map_err(|_| "cannot read secret file")?;
    let meta = file.metadata().map_err(|_| "cannot read secret file")?;
    if !meta.is_file() || meta.len() > 4096 || meta.permissions().mode() & 0o7177 != 0 {
        return Err("secret must be a regular file, <=4096 bytes, with mode 0600 or stricter");
    }
    // Inspect and read the same descriptor; bound the read even if the file grows.
    let mut text = String::new();
    file.take(4097)
        .read_to_string(&mut text)
        .map_err(|_| "cannot read secret file")?;
    if text.len() > 4096 {
        return Err("secret exceeds 4096 bytes");
    }
    let secret = text.trim_end_matches(['\r', '\n']).to_string();
    if secret.is_empty() || secret.contains(['\r', '\n']) {
        return Err("secret must be a nonempty single line");
    }
    Ok(secret)
}
