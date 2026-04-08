//! Settings — parsed from environment variables.
//!
//! voice/server intentionally has very few env vars.
//! All provider (STT/LLM/TTS), telephony, and public URL settings are stored
//! in the `provider_configs` table and read from the DB at startup.
//!
//! | Variable            | Default                    | Description                                       |
//! |---------------------|----------------------------|---------------------------------------------------|
//! | `SERVER__LISTEN_HOST`       | `0.0.0.0`                  | Bind address                                      |
//! | `SERVER__LISTEN_PORT`       | `8300`                     | Bind port                                         |
//! | `DATABASE__URL`     | *(required)*               | Postgres connection string (shared with studio/api)   |
//! | `AUTH__SECRET_KEY`  | *(empty — dev mode)*       | HMAC key; must match studio/api's auth secret         |
//! | `RECORDING__OUTPUT_URI` | `file://./recordings`   | Recording destination URI (see below)             |
//!
//! ## Recording Output URI
//!
//! `RECORDING__OUTPUT_URI` controls where session audio is stored and what URL
//! ends up in the `calls.recording_url` database column.
//!
//! | URI Scheme                  | Stored in DB (Verbatim)        | Resolved by API Layer     |
//! |-----------------------------|--------------------------------|---------------------------|
//! | `file://./recordings`       | `file:///abs/path/{id}.opus`   | Proxied by backend        |
//! | `file:///abs/path`          | `file:///abs/path/{id}.opus`   | Proxied by backend        |
//! | `s3://bucket/prefix`        | `s3://bucket/prefix/{id}.opus` | Pre-signed URL / CDN      |
//!
//! For **bare metal**: set `RECORDING__OUTPUT_URI` to `file:///shared/path` where
//! both voice/server and studio/api have filesystem read/write access.
//! The backend requires no configuration since it reads the absolute path
//! verbatim from the stored URI.
//!
//! For **Docker Compose**: mount a shared volume to `/recordings` in both
//! services. Use `RECORDING__OUTPUT_URI=file:///recordings` (absolute inside container).
//!
//! The public URL (`voice_server_url`) is read from the telephony row in
//! `provider_configs` at startup, so it can be changed in the Settings UI
//! without redeploying.

use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct Settings {
    // ── Server ───────────────────────────────────────────────────
    #[serde(rename = "server__listen_host", default = "default_host")]
    pub listen_host: String,

    #[serde(rename = "server__listen_port", default = "default_port")]
    pub listen_port: u16,

    // ── Database ─────────────────────────────────────────────────
    /// Postgres connection string — the same DB studio/api uses.
    #[serde(rename = "database__url")]
    /// Canonical env name: `DATABASE__URL`.
    pub database_url: String,

    /// HMAC-SHA256 secret for signing/verifying session tokens.
    /// Must match `AUTH__SECRET_KEY` used by studio/api.
    #[serde(rename = "auth__secret_key")]
    /// Canonical env name: `AUTH__SECRET_KEY`.
    #[serde(default)]
    pub auth_secret_key: String,

    // ── Recording ─────────────────────────────────────────────────
    /// Destination URI for session recordings.
    ///
    /// Supported schemes:
    /// - `file:///absolute/path`  — write to absolute local path
    /// - `file://./relative/path` — write relative to the working directory
    /// - `s3://bucket/prefix`     — upload to S3 (requires `s3` Cargo feature)
    ///
    /// studio/api serves recorded files at `/api/recordings/{filename}`.
    /// Both processes must access the same filesystem path for `file://` URIs
    /// (shared Docker volume, NFS, or same bare-metal host).
    #[serde(
        rename = "recording__output_uri",
        default = "default_recording_output_uri"
    )]
    pub recording_output_uri: String,
}

/// Manual Debug impl — redacts sensitive fields so they never appear in logs.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let safe_db = redact_db_url(&self.database_url);
        f.debug_struct("Settings")
            .field("listen_host", &self.listen_host)
            .field("listen_port", &self.listen_port)
            .field("database_url", &safe_db)
            .field(
                "auth_secret_key",
                &if self.auth_secret_key.is_empty() {
                    "<not set>"
                } else {
                    "<set>"
                },
            )
            .field("recording_output_uri", &self.recording_output_uri)
            .finish()
    }
}

impl Settings {
    pub fn from_env() -> Result<Self, envy::Error> {
        let _ = dotenvy::dotenv();
        envy::from_env::<Self>()
    }
}

/// Return the DB URL with credentials stripped.
/// `postgres://user:pass@host:5432/db` → `postgres://<redacted>@host:5432/db`
fn redact_db_url(url: &str) -> String {
    if let Some(after_scheme) = url.split_once("://").map(|(_, rest)| rest) {
        if let Some(at_pos) = after_scheme.rfind('@') {
            let host_and_rest = &after_scheme[at_pos + 1..];
            let scheme = url.split_once("://").map(|(s, _)| s).unwrap_or("postgres");
            return format!("{}://<redacted>@{}", scheme, host_and_rest);
        }
    }
    if let Some(base) = url.split('?').next() {
        base.to_string()
    } else {
        url.to_string()
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    8300
}
fn default_recording_output_uri() -> String {
    "file://./recordings".to_string()
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn from_env_reads_double_underscore_keys() {
        let db_key = "DATABASE__URL";
        let secret_key = "AUTH__SECRET_KEY";
        let old_db = std::env::var(db_key).ok();
        let old_secret = std::env::var(secret_key).ok();

        std::env::set_var(db_key, "postgresql://user:pass@db:5432/voice_agent");
        std::env::set_var(secret_key, "test-secret");

        let settings = Settings::from_env().expect("expected double underscore env vars to parse");
        assert_eq!(
            settings.database_url,
            "postgresql://user:pass@db:5432/voice_agent"
        );
        assert_eq!(settings.auth_secret_key, "test-secret");

        match old_db {
            Some(value) => std::env::set_var(db_key, value),
            None => std::env::remove_var(db_key),
        }
        match old_secret {
            Some(value) => std::env::set_var(secret_key, value),
            None => std::env::remove_var(secret_key),
        }
    }
}
