use crate::{accounts::file_name_for_account, session::SessionData};
use serde::Serialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Serialize)]
pub struct SavedAccount {
    id: String,
    name: String,
    enabled: bool,
    model: Option<String>,
    proxy: Option<String>,
    #[serde(skip)]
    path: PathBuf,
}

pub struct AccountStore {
    root: PathBuf,
}

impl AccountStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn named_account(id: &str) -> Result<String, String> {
        let name = id.strip_prefix("named:").ok_or("Invalid account ID")?;
        let name = file_name_for_account(name)?;
        let device = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (device.len() == 4
                && (device.starts_with("COM") || device.starts_with("LPT"))
                && device.as_bytes()[3].is_ascii_digit())
        {
            return Err("Windows device names are reserved".into());
        }
        Ok(name)
    }

    pub fn profile_dir(id: &str, exe: &Path) -> Result<PathBuf, String> {
        let parent = exe.parent().ok_or("Executable has no parent directory")?;
        let name = if id == "legacy" {
            "legacy.profile".into()
        } else {
            format!("account-{}.profile", Self::named_account(id)?)
        };
        Ok(parent.join("desktop-profiles").join(name))
    }

    fn path(&self, id: &str, enabled: bool) -> Result<PathBuf, String> {
        let suffix = if enabled { ".json" } else { ".json.disabled" };
        if id == "legacy" {
            return Ok(self.root.join(format!("session{suffix}")));
        }
        let name = Self::named_account(id)?;
        Ok(self.root.join("sessions").join(format!("{name}{suffix}")))
    }

    pub fn list(&self) -> Result<Vec<SavedAccount>, String> {
        let mut accounts = Vec::new();
        for enabled in [true, false] {
            let path = self.path("legacy", enabled)?;
            if path.is_file() {
                accounts.push(Self::entry(
                    "legacy".into(),
                    "default (旧版)".into(),
                    enabled,
                    &path,
                ));
            }
        }
        let files = match fs::read_dir(self.root.join("sessions")) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(accounts),
            Err(error) => return Err(error.to_string()),
        };
        for file in files {
            let file = file.map_err(|error| error.to_string())?;
            if !file
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
            {
                continue;
            }
            let filename = file.file_name();
            let Some(filename) = filename.to_str() else {
                continue;
            };
            let lower = filename.to_ascii_lowercase();
            for (suffix, enabled) in [(".json", true), (".json.disabled", false)] {
                if !lower.ends_with(suffix) {
                    continue;
                }
                let name = &filename[..filename.len() - suffix.len()];
                let id = format!("named:{name}");
                if self.path(&id, enabled).is_ok_and(|path| {
                    path.file_name().is_some_and(|canonical| {
                        if cfg!(windows) {
                            canonical.to_string_lossy().eq_ignore_ascii_case(filename)
                        } else {
                            canonical == file.file_name()
                        }
                    })
                }) {
                    accounts.push(Self::entry(id, name.into(), enabled, &file.path()));
                }
            }
        }
        accounts.sort_by(|a, b| a.name.cmp(&b.name).then(b.enabled.cmp(&a.enabled)));
        Ok(accounts)
    }

    fn entry(id: String, name: String, enabled: bool, path: &Path) -> SavedAccount {
        let session = SessionData::load_from(path)
            .ok()
            .filter(|session| !session.cookie.is_empty() && !session.snlm0e.is_empty());
        SavedAccount {
            id,
            name,
            enabled,
            model: session.as_ref().map(|session| session.model.clone()),
            proxy: session.and_then(|session| session.proxy),
            path: path.to_path_buf(),
        }
    }

    /// Set or clear the egress proxy of one saved account.
    pub fn set_proxy(&self, id: &str, proxy: Option<&str>) -> Result<(), String> {
        let path = self.login_path(id)?;
        let mut session = SessionData::load_from(&path)?;
        session.proxy = proxy
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        session.save()
    }

    /// Spread all saved accounts over local Clash listeners.
    ///
    /// Accounts are ordered by name. `per_port` accounts share one listener,
    /// which gives them the same public address. Returns the name and proxy of
    /// every account that was written.
    pub fn assign_proxies(
        &self,
        start_port: u16,
        per_port: u16,
    ) -> Result<Vec<(String, String)>, String> {
        if per_port == 0 {
            return Err("Accounts per port must be at least 1".into());
        }
        let mut accounts = self.list()?;
        accounts.sort_by(|a, b| a.name.cmp(&b.name).then(b.enabled.cmp(&a.enabled)));
        let mut assignments = Vec::with_capacity(accounts.len());
        for (index, account) in accounts.iter().enumerate() {
            let offset = u32::from(start_port) + (index as u32 / u32::from(per_port));
            if offset > u32::from(u16::MAX) {
                return Err("Proxy port range exceeds 65535".into());
            }
            let proxy = format!("http://127.0.0.1:{offset}");
            let mut session = SessionData::load_from(&account.path)?;
            session.proxy = Some(proxy.clone());
            session.save()?;
            assignments.push((account.name.clone(), proxy));
        }
        Ok(assignments)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let source = self.path(id, !enabled)?;
        let destination = self.path(id, enabled)?;
        if destination
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            return Err("A session with that name already exists at the destination".into());
        }
        fs::rename(source, destination).map_err(|error| error.to_string())
    }

    pub fn remove(&self, id: &str, enabled: bool) -> Result<(), String> {
        fs::remove_file(self.path(id, enabled)?).map_err(|error| error.to_string())
    }

    pub fn login_path(&self, id: &str) -> Result<PathBuf, String> {
        let active = self.path(id, true)?;
        let disabled = self.path(id, false)?;
        if disabled.try_exists().map_err(|error| error.to_string())?
            && !active.try_exists().map_err(|error| error.to_string())?
        {
            Ok(disabled)
        } else {
            Ok(active)
        }
    }
}
