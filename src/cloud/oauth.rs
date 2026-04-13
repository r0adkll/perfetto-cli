use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::db::Database;

/// Port for the ephemeral OAuth redirect listener (avoids 9001 used by ui_server).
const REDIRECT_PORT: u16 = 9010;

/// Google OAuth endpoints.
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Settings key prefix for a given provider.
fn key(provider_id: &str, field: &str) -> String {
    format!("cloud.{provider_id}.{field}")
}

/// PKCE code verifier: 43-128 chars from the unreserved set.
pub fn generate_code_verifier() -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random::<u8>()).collect();
    URL_SAFE_NO_PAD.encode(&bytes)
}

/// PKCE S256 code challenge derived from the verifier.
pub fn generate_code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Random state parameter for CSRF protection.
pub fn generate_state() -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..16).map(|_| rng.random::<u8>()).collect();
    URL_SAFE_NO_PAD.encode(&bytes)
}

/// Build the Google authorization URL.
pub fn build_auth_url(client_id: &str, code_challenge: &str, state: &str) -> String {
    let redirect_uri = format!("http://127.0.0.1:{REDIRECT_PORT}/callback");
    format!(
        "{AUTH_URL}?\
         client_id={client_id}\
         &redirect_uri={redirect_uri}\
         &response_type=code\
         &scope=https://www.googleapis.com/auth/drive.file\
         &code_challenge={code_challenge}\
         &code_challenge_method=S256\
         &state={state}\
         &access_type=offline\
         &prompt=consent"
    )
}

/// Start a local HTTP server, wait for the OAuth redirect, and return the
/// authorization code. Validates the `state` parameter.
pub fn wait_for_redirect(expected_state: &str) -> Result<String> {
    let addr = format!("127.0.0.1:{REDIRECT_PORT}");
    let server = tiny_http::Server::http(&addr)
        .map_err(|e| anyhow::anyhow!("failed to bind OAuth listener on {addr}: {e}"))?;

    tracing::info!("OAuth redirect listener started on {addr}");

    loop {
        let request = match server.recv() {
            Ok(r) => r,
            Err(e) => bail!("OAuth listener recv error: {e}"),
        };

        let url = request.url().to_string();
        if !url.starts_with("/callback") {
            let resp = tiny_http::Response::from_string("not found")
                .with_status_code(404);
            let _ = request.respond(resp);
            continue;
        }

        // Parse query params from the callback URL.
        let query = url.splitn(2, '?').nth(1).unwrap_or("");
        let params: Vec<(&str, &str)> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .collect();

        let state = params.iter().find(|(k, _)| *k == "state").map(|(_, v)| *v);
        let code = params.iter().find(|(k, _)| *k == "code").map(|(_, v)| *v);
        let error = params.iter().find(|(k, _)| *k == "error").map(|(_, v)| *v);

        // Respond to the browser immediately.
        let html = if error.is_some() {
            "<html><body><h2>Authorization failed</h2><p>You can close this tab.</p></body></html>"
        } else {
            "<html><body><h2>Authorization successful</h2><p>You can close this tab and return to perfetto-cli.</p></body></html>"
        };
        let resp = tiny_http::Response::from_string(html)
            .with_header("Content-Type: text/html".parse::<tiny_http::Header>().unwrap());
        let _ = request.respond(resp);

        if let Some(err) = error {
            bail!("OAuth error: {err}");
        }

        match (state, code) {
            (Some(s), Some(c)) if s == expected_state => {
                return Ok(c.to_string());
            }
            (Some(_), Some(_)) => {
                bail!("OAuth state mismatch — possible CSRF");
            }
            _ => {
                bail!("OAuth callback missing code or state parameter");
            }
        }
    }
}

/// Token response from Google's token endpoint.
#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
}

/// Exchange an authorization code for access + refresh tokens.
pub async fn exchange_code(
    client_id: &str,
    client_secret: &str,
    code: &str,
    code_verifier: &str,
    db: &Database,
    provider_id: &str,
) -> Result<()> {
    let redirect_uri = format!("http://127.0.0.1:{REDIRECT_PORT}/callback");
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("code_verifier", code_verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await
        .context("token exchange request failed")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("token exchange failed: {body}");
    }

    let token: TokenResponse = resp.json().await.context("failed to parse token response")?;
    store_tokens(db, provider_id, &token)?;
    Ok(())
}

/// Refresh an expired access token using the stored refresh token.
pub async fn refresh_access_token(
    client_id: &str,
    client_secret: &str,
    db: &Database,
    provider_id: &str,
) -> Result<String> {
    let refresh_token = db
        .get_setting(&key(provider_id, "refresh_token"))?
        .context("no refresh token stored")?;

    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .context("token refresh request failed")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("token refresh failed: {body}");
    }

    let token: TokenResponse = resp.json().await.context("failed to parse refresh response")?;
    store_tokens(db, provider_id, &token)?;
    Ok(token.access_token)
}

/// Returns a valid access token, refreshing if expired.
pub async fn ensure_valid_token(
    client_id: &str,
    client_secret: &str,
    db: &Database,
    provider_id: &str,
) -> Result<String> {
    let access_token = db.get_setting(&key(provider_id, "access_token"))?;
    let expires_at = db.get_setting(&key(provider_id, "expires_at"))?;

    if let (Some(token), Some(exp)) = (access_token, expires_at) {
        if let Ok(exp_dt) = exp.parse::<DateTime<Utc>>() {
            // Refresh 60s before actual expiry to avoid race.
            if Utc::now() < exp_dt - Duration::seconds(60) {
                return Ok(token);
            }
        }
    }

    tracing::info!("access token expired or missing, refreshing");
    refresh_access_token(client_id, client_secret, db, provider_id).await
}

/// Persist token fields to the settings table.
fn store_tokens(db: &Database, provider_id: &str, token: &TokenResponse) -> Result<()> {
    db.set_setting(
        &key(provider_id, "access_token"),
        &token.access_token,
    )?;
    if let Some(ref rt) = token.refresh_token {
        db.set_setting(&key(provider_id, "refresh_token"), rt)?;
    }
    let expires_at = Utc::now() + Duration::seconds(token.expires_in);
    db.set_setting(
        &key(provider_id, "expires_at"),
        &expires_at.to_rfc3339(),
    )?;
    Ok(())
}

/// Remove all stored tokens for a provider.
pub fn clear_tokens(db: &Database, provider_id: &str) -> Result<()> {
    db.delete_setting(&key(provider_id, "access_token"))?;
    db.delete_setting(&key(provider_id, "refresh_token"))?;
    db.delete_setting(&key(provider_id, "expires_at"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_code_challenge_is_deterministic_for_verifier() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = generate_code_challenge(verifier);
        let challenge2 = generate_code_challenge(verifier);
        assert_eq!(challenge, challenge2);
        // Should be base64url-encoded, no padding.
        assert!(!challenge.contains('='));
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
    }

    #[test]
    fn code_verifier_is_valid_length() {
        let verifier = generate_code_verifier();
        assert!(verifier.len() >= 43);
        assert!(verifier.len() <= 128);
    }

    #[test]
    fn state_is_nonempty() {
        let state = generate_state();
        assert!(!state.is_empty());
    }

    #[test]
    fn auth_url_contains_required_params() {
        let url = build_auth_url("my_client_id", "my_challenge", "my_state");
        assert!(url.contains("client_id=my_client_id"));
        assert!(url.contains("code_challenge=my_challenge"));
        assert!(url.contains("state=my_state"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("drive.file"));
    }
}
