//! OpenAI device authorization flow and browser-based OAuth2 + PKCE login.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use crate::logging::{STDERR_USER_OUTPUT_TARGET, STDOUT_USER_OUTPUT_TARGET};
use anyhow::{Context, Result, anyhow};
use axum::Router;
use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::get;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use tokio::sync::oneshot;
use tokio::time::sleep;
use tracing::{info, warn};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_AUTH_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const POLL_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
const REFRESH_WINDOW_SECONDS: u64 = 300;

const BROWSER_AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
const BROWSER_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
// Pre-encoded forms used in the authorization URL query string.
const BROWSER_REDIRECT_URI_ENC: &str = "http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback";
const BROWSER_SCOPES_ENC: &str = "openid%20profile%20email%20offline_access";

const BROWSER_LOGIN_TIMEOUT_SECS: u64 = 5 * 60;

const SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html>
<head><title>Login Successful</title></head>
<body style="font-family:sans-serif;text-align:center;padding:2rem;">
<h1>&#10003; Login successful</h1>
<p>You can close this tab and return to the terminal.</p>
</body>
</html>"#;

const ERROR_HTML: &str = r#"<!DOCTYPE html>
<html>
<head><title>Login Failed</title></head>
<body style="font-family:sans-serif;text-align:center;padding:2rem;">
<h1>&#10007; Login failed</h1>
<p>Authorization was denied or an error occurred. Check the terminal for details.</p>
</body>
</html>"#;

/// Stored OAuth credentials and refresh metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    /// Short-lived bearer token used for API calls.
    pub access_token: String,
    /// Refresh token used to mint a new access token.
    pub refresh_token: String,
    /// Opaque token returned by the provider for identity use.
    pub id_token: String,
    /// Unix timestamp (seconds) when `access_token` expires.
    pub expires_at: u64,
}

/// Resolve the auth token file path from `$HOME` with a safe fallback.
pub fn token_file_path() -> PathBuf {
    crate::paths::default_auth_file_path()
}

/// Read persisted tokens from disk.
pub fn read_tokens() -> Result<AuthTokens> {
    let path = token_file_path();
    if !path.exists() {
        return Err(anyhow!("auth file not found at {}", path.display()));
    }

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str::<AuthTokens>(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))
}

/// Run the browser-based OAuth2 + PKCE authorization code flow.
///
/// Flow:
/// 1) Generate a PKCE pair (code_verifier + code_challenge),
/// 2) Start a local HTTP callback server on port 1455,
/// 3) Open the authorization URL in the default browser,
/// 4) Wait for the callback (5 minute timeout), then
/// 5) Exchange the authorization code for access/refresh tokens.
pub async fn browser_login() -> Result<AuthTokens> {
    let (code_verifier, code_challenge) = pkce_pair();
    let state_param = uuid::Uuid::new_v4().to_string();

    let auth_url = format!(
        "{BROWSER_AUTH_URL}?response_type=code&client_id={CLIENT_ID}\
         &redirect_uri={BROWSER_REDIRECT_URI_ENC}\
         &scope={BROWSER_SCOPES_ENC}\
         &state={state_param}\
         &code_challenge={code_challenge}\
         &code_challenge_method=S256",
    );

    let client = Client::new();

    let (callback_tx, callback_rx) = oneshot::channel::<Result<(String, String), String>>();
    let sender: CallbackSender = Arc::new(Mutex::new(Some(callback_tx)));

    let router = Router::new()
        .route("/auth/callback", get(browser_callback_handler))
        .with_state(Arc::clone(&sender));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:1455")
        .await
        .context("failed to bind to port 1455 for OAuth callback — is another process using it?")?;

    let server_handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    info!(target: STDOUT_USER_OUTPUT_TARGET, "Opening browser for authorization...");
    info!(
        target: STDOUT_USER_OUTPUT_TARGET,
        "If the browser does not open, visit:\n  {auth_url}"
    );
    try_open_browser(&auth_url);

    let callback_result = tokio::time::timeout(
        StdDuration::from_secs(BROWSER_LOGIN_TIMEOUT_SECS),
        callback_rx,
    )
    .await
    .context("browser login timed out after 5 minutes")?
    .context("callback channel closed before receiving a response")?
    .map_err(|e| anyhow!("{e}"))?;

    server_handle.abort();

    let (code, received_state) = callback_result;
    if received_state != state_param {
        return Err(anyhow!(
            "OAuth state parameter mismatch — possible CSRF; try logging in again"
        ));
    }

    info!(
        target: STDOUT_USER_OUTPUT_TARGET,
        "Authorization code received, exchanging for tokens..."
    );

    let tokens = exchange_browser_code(&client, code, code_verifier).await?;
    save_tokens(&tokens)?;
    Ok(tokens)
}

/// Run the device-code OAuth flow used by `autopoiesis auth login`.
///
/// Flow:
/// 1) request a user code from the auth server,
/// 2) ask the user to open the verification URL,
/// 3) poll until authorization succeeds, then
/// 4) exchange the returned code for access/refresh tokens.
pub async fn device_code_login() -> Result<AuthTokens> {
    let client = Client::new();

    let response: DeviceCodeResponse =
        post_json(&client, DEVICE_AUTH_URL, &json!({"client_id": CLIENT_ID})).await?;

    let user_code = response
        .user_code
        .ok_or_else(|| anyhow!("user code is missing from device auth response"))?;
    let interval = response.interval.unwrap_or(5).max(1);

    info!(
        target: STDOUT_USER_OUTPUT_TARGET,
        "Open {VERIFICATION_URL} and enter code: {}",
        format_user_code(&user_code)
    );
    info!(
        target: STDOUT_USER_OUTPUT_TARGET,
        "Waiting for device authorization..."
    );

    let authorization =
        poll_for_authorization(&client, &response.device_auth_id, &user_code, interval).await?;
    let tokens = exchange_authorization_code(
        &client,
        authorization.authorization_code,
        authorization.code_verifier,
    )
    .await?;

    save_tokens(&tokens)?;
    Ok(tokens)
}

/// Refresh stored tokens using a refresh token.
pub async fn refresh_tokens(refresh_token: &str) -> Result<AuthTokens> {
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", refresh_token),
    ];

    let tokens = request_token(&Client::new(), &form).await?;
    save_tokens(&tokens)?;
    Ok(tokens)
}

/// Return a valid access token, refreshing it if it is near expiry.
pub async fn get_valid_token() -> Result<String> {
    let tokens = read_tokens().context("no stored token found; run: autopoiesis auth login")?;

    if token_is_near_expiry(tokens.expires_at)? {
        let refreshed = refresh_tokens(&tokens.refresh_token)
            .await
            .map_err(|error| {
                warn!(
                    target: STDERR_USER_OUTPUT_TARGET,
                    "Failed to refresh token: {error}"
                );
                warn!(
                    target: STDERR_USER_OUTPUT_TARGET,
                    "Run: autopoiesis auth login"
                );
                error
            })?;

        return Ok(refreshed.access_token);
    }

    Ok(tokens.access_token)
}

async fn poll_for_authorization(
    client: &Client,
    device_auth_id: &str,
    user_code: &str,
    interval: u64,
) -> Result<AuthorizationResponse> {
    let timeout = StdDuration::from_secs(15 * 60);
    let mut elapsed = StdDuration::from_secs(0);

    loop {
        if elapsed >= timeout {
            info!(target: STDOUT_USER_OUTPUT_TARGET, "");
            return Err(anyhow!("authorization timed out after 15 minutes"));
        }

        // Polling too aggressively can return transient errors; small interval keeps UX responsive.
        let poll_body = json!({
            "device_auth_id": device_auth_id,
            "user_code": user_code,
        });

        let response = client
            .post(POLL_URL)
            .json(&poll_body)
            .send()
            .await
            .context("failed to poll device authorization endpoint")?;

        match response.status() {
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => {
                print!(".");
                io::stdout()
                    .flush()
                    .context("failed to print poll progress")?;
                sleep(StdDuration::from_secs(interval)).await;
                elapsed += StdDuration::from_secs(interval);
            }
            StatusCode::OK => {
                info!(target: STDOUT_USER_OUTPUT_TARGET, "");
                let body = response
                    .json::<AuthorizationResponse>()
                    .await
                    .context("failed to parse authorization response")?;
                return Ok(body);
            }
            _ => {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| String::from("<failed to read response body>"));
                info!(target: STDOUT_USER_OUTPUT_TARGET, "");
                return Err(anyhow!("authorization request failed ({status}): {body}"));
            }
        }
    }
}

async fn request_token(client: &Client, form: &[(&str, &str)]) -> Result<AuthTokens> {
    let response = client
        .post(OAUTH_TOKEN_URL)
        .form(form)
        .send()
        .await
        .context("failed to request token")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("<failed to read response body>"));
        return Err(anyhow!("OAuth token request failed ({status}): {body}"));
    }

    let token_response: TokenExchangeResponse = response
        .json()
        .await
        .context("failed to parse OAuth token response")?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?
        .as_secs();
    let expires_in_seconds = token_response.expires_in.max(0) as u64;
    let expires_at = now.saturating_add(expires_in_seconds);

    Ok(AuthTokens {
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        id_token: token_response.id_token,
        expires_at,
    })
}

async fn exchange_authorization_code(
    client: &Client,
    authorization_code: String,
    code_verifier: String,
) -> Result<AuthTokens> {
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", &authorization_code),
        ("code_verifier", &code_verifier),
        (
            "redirect_uri",
            "https://auth.openai.com/deviceauth/callback",
        ),
    ];

    request_token(client, &form).await
}

// ---------------------------------------------------------------------------
// Browser login helpers
// ---------------------------------------------------------------------------

/// Test whether `auth.openai.com` is reachable from this process's network
/// path.  Fails fast if the server returns a geo-restriction error, which
/// means the tunnel (sshuttle or otherwise) is not routing traffic correctly.
///
/// Uses a no-redirect client so we see the raw 302 vs 403 from the auth
/// endpoint without chasing the login-page redirect.
/// Channel type carrying the callback result: Ok((code, state)) or Err(error message).
type CallbackSender = Arc<Mutex<Option<oneshot::Sender<Result<(String, String), String>>>>>;

#[derive(Deserialize)]
struct BrowserCallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Axum route handler for `/auth/callback`.  Sends the authorization result
/// through the oneshot channel and returns a browser-friendly HTML page.
async fn browser_callback_handler(
    Query(params): Query<BrowserCallbackParams>,
    State(tx): State<CallbackSender>,
) -> Html<&'static str> {
    let result = match (params.code, params.state, params.error) {
        (Some(code), Some(state), _) => Ok((code, state)),
        (_, _, Some(error)) => Err(format!("authorization denied: {error}")),
        _ => Err("missing code or state in OAuth callback".to_string()),
    };

    let html = if result.is_ok() {
        SUCCESS_HTML
    } else {
        ERROR_HTML
    };

    if let Ok(mut guard) = tx.lock()
        && let Some(sender) = guard.take()
    {
        let _ = sender.send(result);
    }

    Html(html)
}

/// Exchange a browser authorization code (PKCE flow) for tokens.
///
/// Uses `http://localhost:1455/auth/callback` as the redirect URI to match the
/// authorization request.
async fn exchange_browser_code(
    client: &Client,
    code: String,
    code_verifier: String,
) -> Result<AuthTokens> {
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code.as_str()),
        ("code_verifier", code_verifier.as_str()),
        ("redirect_uri", BROWSER_REDIRECT_URI),
    ];
    request_token(client, &form).await
}

/// Generate a PKCE code_verifier + code_challenge pair (S256 method).
///
/// `code_verifier`: 32 cryptographically random bytes, base64url-encoded (43 chars).
/// `code_challenge`: SHA-256 of the verifier, base64url-encoded.
fn pkce_pair() -> (String, String) {
    let mut bytes = [0u8; 32];
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    bytes[..16].copy_from_slice(a.as_bytes());
    bytes[16..].copy_from_slice(b.as_bytes());
    let code_verifier = base64url_encode(&bytes);
    let hash = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64url_encode(&hash);
    (code_verifier, code_challenge)
}

/// Base64url encoding (no padding) per RFC 4648 §5.
fn base64url_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        }
    }
    out
}

/// Attempt to open a URL in the system default browser.  Never fails — if no
/// browser launcher is found the user can open the URL printed to the terminal.
///
/// If `ALL_PROXY` or `HTTPS_PROXY` is set in the environment, Chromium/Chrome
/// is launched directly with `--proxy-server` so the auth page goes through the
/// same tunnel as the reqwest HTTP client (which picks up those env vars
/// automatically).
fn try_open_browser(url: &str) {
    // reqwest already honours ALL_PROXY / HTTPS_PROXY; mirror that for the
    // browser so both sides of the flow go through the same tunnel.
    let proxy = std::env::var("ALL_PROXY")
        .or_else(|_| std::env::var("HTTPS_PROXY"))
        .ok();

    if let Some(ref proxy_url) = proxy {
        // Chromium-family browsers accept --proxy-server on the command line.
        let chromium_bins = [
            "chromium-browser",
            "chromium",
            "google-chrome",
            "google-chrome-stable",
            "brave-browser",
        ];
        for bin in chromium_bins {
            if std::process::Command::new(bin)
                .arg(format!("--proxy-server={proxy_url}"))
                .arg("--new-window")
                .arg(url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return;
            }
        }
        // Firefox doesn't expose a usable --proxy CLI flag; fall through to
        // xdg-open and let the system proxy settings handle it.
    }

    #[cfg(target_os = "macos")]
    let launcher = "open";
    #[cfg(target_os = "windows")]
    let launcher = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let launcher = "xdg-open";

    let _ = std::process::Command::new(launcher)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn token_is_near_expiry(expires_at: u64) -> Result<bool> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?
        .as_secs();

    Ok(expires_at <= now.saturating_add(REFRESH_WINDOW_SECONDS))
}

fn save_tokens(tokens: &AuthTokens) -> Result<()> {
    save_tokens_at_path(&token_file_path(), tokens)
}

fn save_tokens_at_path(path: &std::path::Path, tokens: &AuthTokens) -> Result<()> {
    use std::io::Write as _;

    let path = path.to_path_buf();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create auth directory")?;
        #[cfg(unix)]
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to set permissions on {}", parent.display()))?;
    }

    let serialized = serde_json::to_string(tokens).context("failed to serialize auth tokens")?;
    let mut file = std::fs::OpenOptions::new();
    file.create(true).truncate(true).write(true);
    #[cfg(unix)]
    file.mode(0o600);
    let mut file = file
        .open(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(serialized.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    Ok(())
}

fn format_user_code(code: &str) -> String {
    if code.len() == 8 {
        format!("{}-{}", &code[0..4], &code[4..])
    } else {
        code.to_string()
    }
}

async fn post_json<T: DeserializeOwned>(
    client: &Client,
    url: &str,
    body: &serde_json::Value,
) -> Result<T> {
    let response = client
        .post(url)
        .json(body)
        .send()
        .await
        .context("failed to send request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("<failed to read response body>"));
        return Err(anyhow!("request failed ({status}): {body}"));
    }

    response.json().await.context("failed to parse response")
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    #[serde(rename = "user_code", alias = "usercode")]
    user_code: Option<String>,
    #[serde(default, deserialize_with = "deserialize_interval")]
    interval: Option<u64>,
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    // OpenAI sends `interval` inconsistently as either a number or a string depending on API path/version.
    match value {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => Ok(n.as_u64()),
        Some(serde_json::Value::String(s)) => s.parse::<u64>().map(Some).map_err(D::Error::custom),
        Some(_) => Ok(None),
    }
}

#[derive(Debug, Deserialize)]
struct AuthorizationResponse {
    authorization_code: String,
    #[serde(rename = "code_challenge", default)]
    _code_challenge: Option<String>,
    code_verifier: String,
}

#[derive(Debug, Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_in: i64,
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;

    fn temp_auth_path(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "autopoiesis_auth_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("auth.json")
    }

    #[test]
    fn save_tokens_writes_json_payload() {
        let path = temp_auth_path("write");
        let tokens = AuthTokens {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            id_token: "id".to_string(),
            expires_at: 123,
        };

        save_tokens_at_path(&path, &tokens).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("\"access_token\":\"access\""));
    }

    #[cfg(unix)]
    #[test]
    fn save_tokens_uses_private_file_permissions() {
        let path = temp_auth_path("permissions");
        let tokens = AuthTokens {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            id_token: "id".to_string(),
            expires_at: 123,
        };

        save_tokens_at_path(&path, &tokens).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
