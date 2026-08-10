// config.rs — on-disk store for user settings: which AI provider is active
// and, per provider, its API key / model / base-url overrides. This is the
// desktop-app equivalent of a browser's localStorage: a plain TOML file
// under the user's config directory, read at startup and written whenever
// the Settings dialog saves a new value.
//
// Precedence for any single provider's key is: that provider's env var
// (e.g. OPENAI_API_KEY) if set, otherwise whatever is saved here. That way
// scripting/CI usage via env vars keeps working unchanged, and this file is
// purely for the interactive GUI case. See `crate::ai` for how providers
// consume this.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

/// Per-provider overrides. All fields optional — an unset field just means
/// "use the provider's built-in default" (see `crate::ai::Provider`).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl ProviderConfig {
    fn is_empty(&self) -> bool {
        self.api_key.is_none() && self.model.is_none() && self.base_url.is_none()
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Which provider the GUI should use by default: "qwen", "openai",
    /// "anthropic", "gemini", or "custom". Falls back to AI_PROVIDER env
    /// var, then "qwen", if unset — see `crate::ai::Provider::active`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_provider: Option<String>,

    #[serde(default, skip_serializing_if = "ProviderConfig::is_empty")]
    pub qwen: ProviderConfig,
    #[serde(default, skip_serializing_if = "ProviderConfig::is_empty")]
    pub openai: ProviderConfig,
    #[serde(default, skip_serializing_if = "ProviderConfig::is_empty")]
    pub anthropic: ProviderConfig,
    #[serde(default, skip_serializing_if = "ProviderConfig::is_empty")]
    pub gemini: ProviderConfig,
    #[serde(default, skip_serializing_if = "ProviderConfig::is_empty")]
    pub custom: ProviderConfig,
}

impl Config {
    /// Mutable access to one provider's saved settings by name. Returns
    /// `None` for an unrecognised name rather than panicking, since the
    /// name may ultimately come from user input.
    pub fn provider_mut(&mut self, name: &str) -> Option<&mut ProviderConfig> {
        match name {
            "qwen" => Some(&mut self.qwen),
            "openai" => Some(&mut self.openai),
            "anthropic" => Some(&mut self.anthropic),
            "gemini" => Some(&mut self.gemini),
            "custom" => Some(&mut self.custom),
            _ => None,
        }
    }

    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        match name {
            "qwen" => Some(&self.qwen),
            "openai" => Some(&self.openai),
            "anthropic" => Some(&self.anthropic),
            "gemini" => Some(&self.gemini),
            "custom" => Some(&self.custom),
            _ => None,
        }
    }
}

fn config_dir() -> Option<PathBuf> {
    // No XDG/dirs crate dependency — HOME is good enough for a single
    // plain file like this.
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("dpp-gui");
    Some(p)
}

fn config_file() -> Option<PathBuf> {
    let mut p = config_dir()?;
    p.push("config.toml");
    Some(p)
}

/// Loads the whole config file. Returns `Config::default()` (all fields
/// unset) if there's no file, no HOME, or the file fails to parse — this
/// module never fails the caller just because settings haven't been saved
/// yet.
pub fn load_config() -> Config {
    let Some(path) = config_file() else { return Config::default() };
    let Ok(content) = fs::read_to_string(path) else { return Config::default() };
    toml::from_str(&content).unwrap_or_default()
}

/// Saves the whole config file, creating the config directory if needed.
/// On Unix, the file is chmod'd 0600 since it may contain plaintext API
/// keys.
pub fn save_config(cfg: &Config) -> io::Result<()> {
    let dir = config_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "could not determine $HOME"))?;
    fs::create_dir_all(&dir)?;
    let path = config_file().expect("config_dir() succeeded above");
    let text = toml::to_string_pretty(cfg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    fs::write(&path, text)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms)?;
    }

    Ok(())
}

/// Convenience: read one provider's saved API key without loading the
/// whole config struct by hand.
pub fn load_api_key(provider: &str) -> Option<String> {
    load_config().provider(provider)?.api_key.clone().filter(|s| !s.is_empty())
}

/// Convenience: read one provider's saved model override, if any.
pub fn load_model(provider: &str) -> Option<String> {
    load_config().provider(provider)?.model.clone().filter(|s| !s.is_empty())
}

/// Convenience: read one provider's saved base-url override, if any.
pub fn load_base_url(provider: &str) -> Option<String> {
    load_config().provider(provider)?.base_url.clone().filter(|s| !s.is_empty())
}

/// Save (overwrite) a single provider's API key, leaving every other
/// provider's settings and the active-provider choice untouched.
pub fn save_api_key(provider: &str, key: &str) -> io::Result<()> {
    let mut cfg = load_config();
    let Some(p) = cfg.provider_mut(provider) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unknown provider {provider}")));
    };
    p.api_key = Some(key.trim().to_string());
    save_config(&cfg)
}

/// Save (overwrite) a single provider's model override. Pass an empty
/// string to clear it back to the built-in default.
pub fn save_model(provider: &str, model: &str) -> io::Result<()> {
    let mut cfg = load_config();
    let Some(p) = cfg.provider_mut(provider) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unknown provider {provider}")));
    };
    p.model = if model.trim().is_empty() { None } else { Some(model.trim().to_string()) };
    save_config(&cfg)
}

/// Save (overwrite) a single provider's base-url override (mainly useful
/// for "custom", i.e. any other OpenAI-compatible endpoint).
pub fn save_base_url(provider: &str, base_url: &str) -> io::Result<()> {
    let mut cfg = load_config();
    let Some(p) = cfg.provider_mut(provider) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unknown provider {provider}")));
    };
    p.base_url = if base_url.trim().is_empty() { None } else { Some(base_url.trim().to_string()) };
    save_config(&cfg)
}

/// Remove a saved API key for one provider (model/base-url overrides, and
/// other providers, are left alone).
pub fn clear_api_key(provider: &str) -> io::Result<()> {
    let mut cfg = load_config();
    if let Some(p) = cfg.provider_mut(provider) {
        p.api_key = None;
    }
    save_config(&cfg)
}

/// The saved default provider (see `Config::active_provider`), if any.
pub fn load_active_provider() -> Option<String> {
    load_config().active_provider.filter(|s| !s.is_empty())
}

/// Save which provider the GUI should default to next time.
pub fn save_active_provider(provider: &str) -> io::Result<()> {
    let mut cfg = load_config();
    cfg.active_provider = Some(provider.to_string());
    save_config(&cfg)
}

/// Where the config file lives, for display in the UI (e.g. "saved to
/// ~/.config/dpp-gui/config.toml").
pub fn config_file_display() -> String {
    config_file()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown: $HOME not set>".to_string())
}
