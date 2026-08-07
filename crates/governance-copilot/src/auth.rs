//! GitHub App authentication: mint an RS256 JWT for the App, then exchange it
//! for a short-lived installation token (RFC-0001). This is a Rust port of the
//! flow verified live in spike-0007 (`docs/spikes/spike-0007-run.sh`).
//!
//! Installation tokens expire after one hour; we never persist one. The App's
//! private key is typed as `RawSecret` so a stray `Debug`/`Display` cannot
//! print it (the never-log-a-secret rule, applied structurally).

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;

use crate::{
    client::GithubClient,
    error::{CopilotError, Result},
    secret::RawSecret,
};

/// Installation tokens expire after one hour; we request well under that.
const TOKEN_TTL_SECS: i64 = 600;

/// Claims for the GitHub App JWT (`iss` = App ID).
#[derive(Debug, Serialize)]
struct AppClaims {
    /// Issued-at (seconds since epoch).
    iat: i64,
    /// Expiry (seconds since epoch).
    exp: i64,
    /// The GitHub App ID.
    iss: String,
}

/// GitHub App credential: App ID (public) + private key (never logged).
pub struct AppAuth<'c> {
    app_id: String,
    private_key: RawSecret,
    client: &'c GithubClient,
}

impl<'c> AppAuth<'c> {
    pub fn new(app_id: String, private_key: RawSecret, client: &'c GithubClient) -> Self {
        Self {
            app_id,
            private_key,
            client,
        }
    }

    /// Mint the App JWT. Only ever sent to GitHub, never logged.
    fn app_jwt(&self) -> Result<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| CopilotError::Token(e.to_string()))?
            .as_secs() as i64;
        let claims = AppClaims {
            iat: now,
            exp: now + TOKEN_TTL_SECS,
            iss: self.app_id.clone(),
        };
        let key = EncodingKey::from_rsa_pem(self.private_key.as_bytes())
            .map_err(|e| CopilotError::Token(format!("private key PEM: {e}")))?;
        encode(&Header::new(Algorithm::RS256), &claims, &key)
            .map_err(|e| CopilotError::Token(e.to_string()))
    }

    async fn get(&self, url: &str, bearer: &RawSecret) -> Result<(u16, serde_json::Value)> {
        let resp = self
            .client
            .send_with_retry(|| {
                self.client
                    .inner()
                    .get(url)
                    .bearer_auth(bearer.as_ref())
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", crate::API_VERSION)
            })
            .await?;
        let status = resp.status().as_u16();
        // `CopilotError::transport`, never the `Transport` variant directly:
        // reqwest's `Error` Display embeds the request URL, which on the
        // download path is the short-lived SIGNED URL (AGENTS.md: never log a
        // signed URL). The constructor strips it via `without_url()`.
        let text = resp.text().await.map_err(CopilotError::transport)?;
        // A 200 whose body is not JSON is a genuinely unexpected shape. A
        // 4xx/5xx with a non-JSON body (e.g. GitHub's plain-text "Request
        // forbidden... User-Agent" page) is reported by status only -- never
        // echo a response body into an error (AGENTS.md: never log a request/
        // response body; `CopilotError::github` is built to take no body).
        match serde_json::from_str(&text) {
            Ok(json) => Ok((status, json)),
            Err(e) if status == 200 => {
                Err(CopilotError::github(kind_for(url), status, e.to_string()))
            }
            Err(_) => Err(CopilotError::github(
                kind_for(url),
                status,
                "unparseable (non-JSON) error body".to_owned(),
            )),
        }
    }

    /// Resolve the numeric installation id for `org` (case-insensitive, as
    /// GitHub treats org logins).
    async fn installation_id(&self, org: &str) -> Result<String> {
        let jwt = RawSecret::new(self.app_jwt()?);
        let url = format!("{}/app/installations", self.client.api_base());
        let (status, json) = self.get(&url, &jwt).await?;
        if status != 200 {
            return Err(CopilotError::github(
                "app/installations",
                status,
                message(&json),
            ));
        }
        let org = org.to_ascii_lowercase();
        for install in json.as_array().unwrap_or(&Vec::new()) {
            let account = install
                .get("account")
                .and_then(|a| a.get("login"))
                .and_then(|l| l.as_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if account != org {
                continue;
            }
            if let Some(id) = install.get("id").and_then(|i| i.as_u64()) {
                return Ok(id.to_string());
            }
        }
        Err(CopilotError::Token(format!(
            "no App installation found for org {org}"
        )))
    }

    /// Exchange the App JWT for a fresh installation token for `org`.
    pub async fn token_for_org(&self, org: &str) -> Result<RawSecret> {
        let id = self.installation_id(org).await?;
        let jwt = RawSecret::new(self.app_jwt()?);
        let url = format!(
            "{}/app/installations/{id}/access_tokens",
            self.client.api_base()
        );
        let resp = self
            .client
            .send_with_retry(|| {
                self.client
                    .inner()
                    .post(&url)
                    .bearer_auth(jwt.as_ref())
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", crate::API_VERSION)
            })
            .await?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(CopilotError::transport)?;
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| CopilotError::Token(format!("token response: {e}")))?;
        if status != 201 {
            return Err(CopilotError::github(
                "app/installations/{id}/access_tokens",
                status,
                message(&json),
            ));
        }
        let token = json
            .get("token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| CopilotError::Token("no token in response".to_owned()))?;
        Ok(RawSecret::new(token.to_owned()))
    }
}

/// Pull the `message` field from a GitHub error JSON body (never log the body).
fn message(json: &serde_json::Value) -> String {
    json.get("message")
        .and_then(|m| m.as_str())
        .map_or_else(|| "no message".to_owned(), str::to_owned)
}

fn kind_for(url: &str) -> &'static str {
    if url.contains("/app/installations") {
        "app/installations"
    } else {
        "github"
    }
}
