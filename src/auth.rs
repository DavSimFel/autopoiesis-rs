use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::{Client, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use tokio::time::sleep;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_AUTH_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const POLL_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: DateTime<Utc>,
}

pub fn token_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".autopoiesis")
        .join("auth.json")
}

pub fn read_tokens() -> Result<AuthTokens> {
    load_tokens()
}

pub async fn device_code_login() -> Result<AuthTokens> {
    let client = Client::new();

    let response: DeviceCodeResponse = post_json(
        &client,
        DEVICE_AUTH_URL,
        &json!({"client_id": CLIENT_ID}),
    )
    .await?;

    let user_code = response
        .user_code
        .ok_or_else(|| anyhow!("user code is missing from device auth response"))?;
    let interval = response.interval.unwrap_or(5).max(1);

    println!(
        "Open {VERIFICATION_URL} and enter code: {}",
        format_user_code(&user_code)
    );
    println!("Waiting for device authorization...");

    let authorization = poll_for_authorization(&client, &response.device_auth_id, &user_code, interval).await?;
    let tokens = exchange_authorization_code(&client, authorization.authorization_code, authorization.code_verifier).await?;

    save_tokens(&tokens)?;
    Ok(tokens)
}

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

pub async fn get_valid_token() -> Result<String> {
    let tokens = read_tokens();

    if tokens.is_err() {
        return Err(anyhow!("Run: autopoiesis auth login"));
    }

    let tokens = tokens?;
    let refresh_window = ChronoDuration::minutes(5);

    if tokens.expires_at <= Utc::now() + refresh_window {
            match refresh_tokens(&tokens.refresh_token).await {
            Ok(refreshed) => return Ok(refreshed.access_token),
            Err(error) => {
                eprintln!("Failed to refresh token: {error}");
                eprintln!("Run: autopoiesis auth login");
                return Err(error);
            }
        }
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
            println!();
            return Err(anyhow!("authorization timed out after 15 minutes"));
        }

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
                io::stdout().flush().context("failed to print poll progress")?;
                sleep(StdDuration::from_secs(interval)).await;
                elapsed += StdDuration::from_secs(interval);
            }
            StatusCode::OK => {
                println!();
                let body = response
                    .json::<AuthorizationResponse>()
                    .await
                    .context("failed to parse authorization response")?;
                return Ok(body);
            }
            _ => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                println!();
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
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("OAuth token request failed ({status}): {body}"));
    }

    let token_response: TokenExchangeResponse = response
        .json()
        .await
        .context("failed to parse OAuth token response")?;

    let expires_at = Utc::now() + ChronoDuration::seconds(token_response.expires_in.max(0));

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
        ("redirect_uri", "https://auth.openai.com/deviceauth/callback"),
    ];

    request_token(client, &form).await
}

fn save_tokens(tokens: &AuthTokens) -> Result<()> {
    let path = token_file_path();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create auth directory")?;
    }

    let serialized = serde_json::to_string(tokens).context("failed to serialize auth tokens")?;
    std::fs::write(&path, serialized)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn load_tokens() -> Result<AuthTokens> {
    let path = token_file_path();

    if !path.exists() {
        return Err(anyhow!("auth file not found at {}", path.display()));
    }

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    serde_json::from_str::<AuthTokens>(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))
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
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("request failed ({status}): {body}"));
    }

    response
        .json()
        .await
        .context("failed to parse response")
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    #[serde(rename = "user_code", alias = "usercode")]
    user_code: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationResponse {
    authorization_code: String,
    #[allow(dead_code)]
    #[serde(default)]
    code_challenge: Option<String>,
    code_verifier: String,
}

#[derive(Debug, Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_in: i64,
}
