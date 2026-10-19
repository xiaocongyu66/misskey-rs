//! config.yaml loading. Mirrors the shape of upstream `src/config.ts`
//! (subset for now; unknown keys are ignored so real upstream configs load).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_url")]
    pub url: String,
    pub db: Option<DbConfig>,
    pub redis: Option<RedisConfig>,
    /// Enables the `POST /_test/reset` hook (e2e harness only).
    #[serde(default)]
    pub test: bool,
    /// server version — upstream config.version resolves from built/meta.json
    /// (config.ts L267-275; the e2e harness writes it from the pinned
    /// package.json). Mirrored for the nodeinfo software.version field;
    /// load() fills it from env/built/meta.json when the yaml omits it.
    #[serde(default)]
    pub version: Option<String>,
}

fn default_port() -> u16 {
    3000
}

fn default_url() -> String {
    "http://localhost:3000".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct DbConfig {
    #[serde(default = "default_db_host")]
    pub host: String,
    #[serde(default = "default_db_port")]
    pub port: u16,
    #[serde(default = "default_db_user")]
    pub user: String,
    #[serde(default)]
    pub pass: Option<String>,
    #[serde(default = "default_db_name")]
    pub db: String,
}

fn default_db_host() -> String {
    "127.0.0.1".to_string()
}

fn default_db_port() -> u16 {
    5432
}

fn default_db_user() -> String {
    "postgres".to_string()
}

fn default_db_name() -> String {
    "postgres".to_string()
}

impl DbConfig {
    /// Builds a postgres DSN. Values with special characters must be
    /// percent-encoded in the yaml (same constraint as upstream's pg URL).
    pub fn url(&self) -> String {
        let mut auth = self.user.clone();
        if let Some(pass) = &self.pass {
            auth.push(':');
            auth.push_str(pass);
        }
        format!(
            "postgres://{}@{}:{}/{}",
            auth, self.host, self.port, self.db
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    #[serde(default = "default_db_host")]
    pub host: String,
    #[serde(default = "default_redis_port")]
    pub port: u16,
    #[serde(default)]
    pub pass: Option<String>,
}

fn default_redis_port() -> u16 {
    6379
}

impl Config {
    /// JS `new URL(config.url).host` — scheme stripped, port kept.
    pub fn host(&self) -> String {
        let rest = self
            .url
            .split_once("://")
            .map(|(_, r)| r)
            .unwrap_or(&self.url);
        rest.split('/').next().unwrap_or(rest).to_string()
    }
}

pub fn load(path: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)?;
    let mut config: Config = serde_yaml::from_str(&raw)?;
    // upstream config.ts version resolution chain: yaml `version:` →
    // built/meta.json (resolve(cwd, 'built/meta.json')) — the harness spawn
    // runs with cwd = upstream/packages/backend, mirroring projectBuiltDir.
    if config.version.is_none() {
        config.version = std::env::var("MISSKEY_RS_VERSION").ok().or_else(|| {
            std::fs::read_to_string("built/meta.json")
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|meta| {
                    meta.get("version")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
        });
    }
    Ok(config)
}
