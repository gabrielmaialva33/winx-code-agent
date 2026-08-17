//! Typed configuration and secret-loading helpers.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::errors::{Result, WinxError};

/// Minimum accepted HTTP bearer-token length in bytes.
pub const MIN_HTTP_TOKEN_BYTES: usize = 32;

/// Parse common human-friendly boolean spellings.
pub fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

/// Read a boolean environment variable with one consistent spelling policy.
pub fn env_flag(name: &str) -> bool {
    std::env::var(name).ok().as_deref().and_then(parse_bool).unwrap_or(false)
}

/// Read and trim a non-empty environment variable.
pub fn env_text(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// Authenticated HTTP principal. Only a SHA-256 token digest is retained.
#[derive(Clone)]
pub struct HttpPrincipal {
    name: String,
    id: String,
    token_digest: [u8; 32],
}

impl std::fmt::Debug for HttpPrincipal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpPrincipal")
            .field("name", &self.name)
            .field("id", &self.id)
            .field("token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl HttpPrincipal {
    pub fn new(name: impl Into<String>, token: &str, allow_weak_token: bool) -> Result<Self> {
        let name = name.into().trim().to_string();
        if name.is_empty() {
            return Err(WinxError::ConfigurationError(
                "HTTP principal name cannot be empty".to_string(),
            ));
        }
        if !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        {
            return Err(WinxError::ConfigurationError(format!(
                "HTTP principal name {name:?} may contain only ASCII letters, digits, '_' and '-'"
            )));
        }
        validate_http_token(token, allow_weak_token)?;
        let token_digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let id_digest = Sha256::digest(name.as_bytes());
        let id = id_digest[..8].iter().fold(String::with_capacity(16), |mut output, byte| {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
            output
        });
        Ok(Self { name, id, token_digest })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn session_prefix(&self) -> String {
        format!("p_{}__", self.id)
    }

    pub fn task_prefix(&self) -> String {
        format!("task_{}__", self.id)
    }

    pub fn matches_token(&self, presented: &str) -> bool {
        let digest: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        constant_time_digest_eq(&self.token_digest, &digest)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalFile {
    principals: Vec<PrincipalEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalEntry {
    name: String,
    token_file: Option<PathBuf>,
    token_env: Option<String>,
}

/// Resolve either a legacy single principal or a multi-principal TOML file.
pub fn load_http_principals(
    principal_config: Option<&Path>,
    command_token: Option<String>,
    command_token_file: Option<&Path>,
    allow_weak_token: bool,
) -> Result<Vec<HttpPrincipal>> {
    if principal_config.is_some() && (command_token.is_some() || command_token_file.is_some()) {
        return Err(WinxError::ConfigurationError(
            "--principal-config cannot be combined with --token/--token-file".to_string(),
        ));
    }

    let principals = if let Some(path) = principal_config {
        let content = std::fs::read_to_string(path).map_err(|error| {
            WinxError::ConfigurationError(format!(
                "cannot read HTTP principal config {}: {error}",
                path.display()
            ))
        })?;
        let config: PrincipalFile = toml::from_str(&content).map_err(|error| {
            WinxError::ConfigurationError(format!(
                "invalid HTTP principal config {}: {error}",
                path.display()
            ))
        })?;
        if config.principals.is_empty() {
            return Err(WinxError::ConfigurationError(
                "HTTP principal config must contain at least one [[principals]] entry".to_string(),
            ));
        }
        config
            .principals
            .into_iter()
            .map(|entry| {
                let token = match (entry.token_file.as_deref(), entry.token_env.as_deref()) {
                    (Some(path), None) => load_secret_file(path)?,
                    (None, Some(name)) => std::env::var(name).map_err(|error| {
                        WinxError::ConfigurationError(format!(
                            "HTTP principal {} token_env {name:?} is unavailable: {error}",
                            entry.name
                        ))
                    })?,
                    (Some(_), Some(_)) => {
                        return Err(WinxError::ConfigurationError(format!(
                            "HTTP principal {} must set exactly one of token_file or token_env",
                            entry.name
                        )))
                    }
                    (None, None) => {
                        return Err(WinxError::ConfigurationError(format!(
                            "HTTP principal {} must set token_file or token_env",
                            entry.name
                        )))
                    }
                };
                HttpPrincipal::new(entry.name, token.trim(), allow_weak_token)
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        let token = match (command_token, command_token_file) {
            (Some(_), Some(_)) => {
                return Err(WinxError::ConfigurationError(
                    "provide only one of --token or --token-file".to_string(),
                ))
            }
            (Some(token), None) => token,
            (None, Some(path)) => load_secret_file(path)?,
            (None, None) => env_text("WINX_HTTP_TOKEN").unwrap_or_default(),
        };
        vec![HttpPrincipal::new("default", token.trim(), allow_weak_token)?]
    };

    reject_duplicate_principals(&principals)?;
    Ok(principals)
}

pub fn load_secret_file(path: &Path) -> Result<String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        WinxError::ConfigurationError(format!(
            "cannot inspect secret file {}: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(WinxError::ConfigurationError(format!(
            "secret path {} must be a regular, non-symlink file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(WinxError::ConfigurationError(format!(
                "secret file {} is readable or writable by group/others; use chmod 600",
                path.display()
            )));
        }
    }
    let value = std::fs::read_to_string(path).map_err(|error| {
        WinxError::ConfigurationError(format!(
            "cannot read secret file {}: {error}",
            path.display()
        ))
    })?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(WinxError::ConfigurationError(format!(
            "secret file {} is empty",
            path.display()
        )));
    }
    Ok(value)
}

fn validate_http_token(token: &str, allow_weak_token: bool) -> Result<()> {
    if token.is_empty() {
        return Err(WinxError::ConfigurationError(
            "refusing to start HTTP transport without a token (RCE exposure)".to_string(),
        ));
    }
    if !allow_weak_token && token.len() < MIN_HTTP_TOKEN_BYTES {
        return Err(WinxError::ConfigurationError(format!(
            "HTTP token is too short ({} bytes); use at least {MIN_HTTP_TOKEN_BYTES} bytes or pass --allow-weak-token for local development",
            token.len()
        )));
    }
    Ok(())
}

fn reject_duplicate_principals(principals: &[HttpPrincipal]) -> Result<()> {
    for (index, principal) in principals.iter().enumerate() {
        for other in principals.iter().skip(index + 1) {
            if principal.name == other.name || principal.id == other.id {
                return Err(WinxError::ConfigurationError(format!(
                    "duplicate HTTP principal name {:?}",
                    principal.name
                )));
            }
            if constant_time_digest_eq(&principal.token_digest, &other.token_digest) {
                return Err(WinxError::ConfigurationError(format!(
                    "HTTP principals {:?} and {:?} use the same token",
                    principal.name, other.name
                )));
            }
        }
    }
    Ok(())
}

fn constant_time_digest_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter().zip(right).fold(0_u8, |difference, (left, right)| difference | (left ^ right)) == 0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{load_http_principals, load_secret_file, parse_bool, HttpPrincipal};

    #[test]
    fn boolean_parser_is_consistent() {
        assert_eq!(parse_bool(" TRUE "), Some(true));
        assert_eq!(parse_bool("yes"), Some(true));
        assert_eq!(parse_bool("off"), Some(false));
        assert_eq!(parse_bool("false"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn principal_keeps_only_a_digest_and_matches_constant_time() {
        let token = "0123456789abcdef0123456789abcdef";
        let principal = HttpPrincipal::new("chatgpt", token, false).expect("valid principal");
        assert!(principal.matches_token(token));
        assert!(!principal.matches_token("0123456789abcdef0123456789abcdeg"));
        assert!(!format!("{principal:?}").contains(token));
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_requires_user_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::TempDir::new().expect("temp directory");
        let secret = directory.path().join("token");
        std::fs::write(&secret, "0123456789abcdef0123456789abcdef\n").expect("write token");
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644))
            .expect("set broad permissions");
        assert!(load_secret_file(&secret).is_err());

        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))
            .expect("set private permissions");
        assert_eq!(
            load_secret_file(&secret).expect("private token"),
            "0123456789abcdef0123456789abcdef"
        );
    }

    #[cfg(unix)]
    #[test]
    fn principal_toml_loads_independent_file_backed_identities() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::TempDir::new().expect("temp directory");
        let left = directory.path().join("left-token");
        let right = directory.path().join("right-token");
        for (path, value) in [
            (&left, "left-0123456789abcdef0123456789abcdef"),
            (&right, "right-0123456789abcdef0123456789abcdef"),
        ] {
            std::fs::write(path, value).expect("write token");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("private token permissions");
        }
        let config = directory.path().join("principals.toml");
        std::fs::write(
            &config,
            format!(
                "[[principals]]\nname = \"left\"\ntoken_file = {left:?}\n\n[[principals]]\nname = \"right\"\ntoken_file = {right:?}\n"
            ),
        )
        .expect("write principal config");

        let principals =
            load_http_principals(Some(&config), None, None, false).expect("load principals");
        assert_eq!(principals.len(), 2);
        assert!(principals[0].matches_token("left-0123456789abcdef0123456789abcdef"));
        assert!(principals[1].matches_token("right-0123456789abcdef0123456789abcdef"));
        assert_ne!(principals[0].session_prefix(), principals[1].session_prefix());
    }
}
