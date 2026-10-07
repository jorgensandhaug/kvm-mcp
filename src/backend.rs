use crate::{Error, Result};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{pin::Pin, time::SystemTime};

pub type ByteStream = Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>;

pub struct Download {
    pub size: u64,
    pub stream: ByteStream,
}
pub struct Snapshot {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub captured_at: SystemTime,
}

#[derive(Debug, Serialize)]
pub struct TargetInfo {
    pub id: u8,
    pub label: String,
}
#[derive(Debug, Serialize)]
pub struct Capabilities {
    pub screenshot: bool,
    pub keyboard: bool,
    pub pointer: bool,
    pub clipboard: bool,
    pub power: bool,
    pub virtual_media: bool,
    pub resumable_transfer: bool,
}
#[derive(Debug, Serialize)]
pub struct KvmStatus {
    pub backend: String,
    pub active_target: u8,
    pub targets: Vec<TargetInfo>,
    pub capabilities: Capabilities,
    /// Explicitly vendor-specific diagnostics; never required for common operations.
    pub diagnostics: Value,
}
#[derive(Debug, Serialize)]
pub struct MediaImage {
    pub name: String,
    pub size_bytes: u64,
    pub complete: Option<bool>,
}
#[derive(Debug, Serialize)]
pub struct MediaCatalog {
    pub enabled: bool,
    pub connected: bool,
    pub mounted_image: Option<String>,
    pub free_bytes: Option<u64>,
    pub images: Vec<MediaImage>,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    /// DOM key codes, e.g. ["ControlLeft", "AltLeft", "Delete"]. Automatically releases.
    Chord {
        keys: Vec<String>,
    },
    /// Keyboard emulation, not OS clipboard access. Specify an installed keymap if needed.
    Text {
        text: String,
        keymap: Option<String>,
    },
    /// Absolute coordinates normalized to [0,1] relative to the full screenshot.
    Move {
        x: f64,
        y: f64,
    },
    Click {
        button: Button,
    },
    Scroll {
        x: i8,
        y: i8,
    },
    ReleaseAll,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Power {
    On,
    Off,
    OffHard,
    ResetHard,
    Button,
    ButtonLong,
    ResetButton,
}

/// Portable operations; never silently succeed for an unsupported hardware feature.
/// Implementations own target identity, serialization, credentials and protocol mapping.
#[async_trait]
pub trait KvmBackend: Send + Sync {
    async fn status(&self) -> Result<KvmStatus>;
    async fn select(&self, target: u8) -> Result<Value>;
    async fn capture(&self, target: u8, max_width: u32) -> Result<Snapshot>;
    async fn input(&self, target: u8, input: Input) -> Result<Value>;
    async fn power(&self, target: u8, action: Power) -> Result<Value>;
    async fn media_list(&self) -> Result<MediaCatalog>;
    async fn media_mount(&self, target: u8, name: &str, cdrom: bool) -> Result<Value>;
    async fn media_eject(&self, target: u8) -> Result<Value>;
    async fn media_delete(&self, name: &str) -> Result<Value>;
    async fn upload(&self, name: &str, size: u64, stream: ByteStream) -> Result<Value>;
    async fn download(&self, name: &str) -> Result<Download>;
}

/// Single flat image name; no paths, hidden files, URL encoding, shell syntax or CRLF.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(Error::Invalid(
            "image must be a flat ASCII filename of 1..128 characters",
        ));
    }
    Ok(())
}

pub fn validate_input(input: &Input) -> Result<()> {
    match input {
        Input::Chord { keys }
            if keys.is_empty()
                || keys.len() > 8
                || keys.iter().any(|k| {
                    k.is_empty() || k.len() > 32 || !k.bytes().all(|b| b.is_ascii_alphanumeric())
                }) =>
        {
            Err(Error::Invalid("supply 1..8 DOM key codes"))
        }
        Input::Text { text, keymap }
            if text.len() > 16384 || keymap.as_ref().is_some_and(|k| validate_name(k).is_err()) =>
        {
            Err(Error::Invalid("text exceeds 16 KiB or keymap is invalid"))
        }
        Input::Move { x, y }
            if !x.is_finite()
                || !y.is_finite()
                || !(0.0..=1.0).contains(x)
                || !(0.0..=1.0).contains(y) =>
        {
            Err(Error::Invalid(
                "coordinates must be finite numbers in [0,1]",
            ))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_not_paths_or_headers() {
        for n in [
            "",
            "../x",
            ".hidden",
            "a/b",
            "a\\b",
            "x%2fy",
            "x\r\nheader",
            "$(id)",
        ] {
            assert!(validate_name(n).is_err(), "{n}");
        }
        assert!(validate_name("linux-1.2_x86.iso").is_ok());
    }
    #[test]
    fn input_is_bounded() {
        assert!(
            validate_input(&Input::Move {
                x: f64::NAN,
                y: 0.0
            })
            .is_err()
        );
        assert!(validate_input(&Input::Move { x: 1.1, y: 0.0 }).is_err());
        assert!(
            validate_input(&Input::Chord {
                keys: vec!["ControlLeft".into(), "KeyA".into()]
            })
            .is_ok()
        );
    }
}
