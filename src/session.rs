use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub snlm0e: String,
    pub f_sid: String,
    pub bl: String,
    pub cookie: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub response_id: Option<String>,
    #[serde(default)]
    pub choice_id: Option<String>,
    /// Optional per-account RPC origin, for example a local test double or a
    /// dedicated egress proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional egress proxy for this account.
    ///
    /// A Clash Verge mixed listener is an HTTP proxy, so a value such as
    /// `http://127.0.0.1:7901` sends every Gemini Web request for this account
    /// through the node that listener is bound to. Accounts that share a port
    /// share a public address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// Optional local source address for this account, for example `127.0.0.7`.
    ///
    /// Every loopback address in `127.0.0.0/8` can be bound on Windows. Clash
    /// Verge selects an egress node with `SRC-IP-CIDR`, so accounts can share
    /// the fixed mixed port and still leave through different nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_ip: Option<String>,
    /// File this session was loaded from. Local machine data, never serialized.
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

fn default_model() -> String {
    "Argon4p0".to_string()
}

impl SessionData {
    pub fn config_dir() -> PathBuf {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        #[cfg(windows)]
        let home = home.or_else(std::env::home_dir);

        home.map(|dir| dir.join(".config/dragoman"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("session.json")
    }

    pub fn sessions_dir() -> PathBuf {
        Self::config_dir().join("sessions")
    }

    pub fn load() -> Result<Self, String> {
        Self::load_from(&Self::config_path())
    }

    pub fn load_from(path: &std::path::Path) -> Result<Self, String> {
        if !path.exists() {
            return Err(format!(
                "Session file not found at {}. Run 'dragoman login' to authenticate with your Google account first.",
                path.display()
            ));
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read session file: {}", e))?;

        let mut session: SessionData = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse session file: {}", e))?;
        session.path = Some(path.to_path_buf());

        Ok(session)
    }

    pub fn save(&self) -> Result<(), String> {
        let path = self.path.clone().unwrap_or_else(Self::config_path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize session: {}", e))?;

        std::fs::write(&path, json).map_err(|e| format!("Failed to write session file: {}", e))?;

        Ok(())
    }
}
