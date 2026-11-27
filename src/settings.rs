use crate::session::SessionData;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Global proxy switches shared by the CLI, the GUI and a running server.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxySettings {
    /// Let `free` accounts answer chat requests in addition to video analysis.
    #[serde(default)]
    pub allow_free_chat: bool,
}

impl ProxySettings {
    pub fn path() -> PathBuf {
        SessionData::config_dir().join("proxy-settings.json")
    }

    /// Read the settings file. A missing or unreadable file means defaults.
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Failed to create config directory: {error}"))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| format!("Failed to encode settings: {error}"))?;
        std::fs::write(&path, text)
            .map_err(|error| format!("Failed to write {}: {error}", path.display()))
    }
}
