//! 微软身份平台的 OAuth 2.0 授权码 + PKCE 流程（公共客户端，不需要客户端密钥）。
//!
//! 流程：在本机回环地址上监听一个随机端口 → 用系统浏览器打开授权页 →
//! 用户登录并同意后，浏览器跳回 `http://localhost:端口/` 并带上授权码 →
//! 用授权码和 `code_verifier` 换取访问令牌和刷新令牌。

use crate::SyncError;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

/// Outlook 账号的连接配置。
#[derive(Debug, Clone)]
pub struct OutlookConfig {
    /// 登录服务基地址：真实为 `https://login.microsoftonline.com`。
    pub authority: String,
    /// 租户：个人账号和组织账号都允许时用 `common`。
    pub tenant: String,
    /// Graph 基地址：真实为 `https://graph.microsoft.com`。
    pub graph: String,
    /// 在 Azure 门户注册的应用（客户端）ID。
    pub client_id: String,
    pub scopes: Vec<String>,
}

impl OutlookConfig {
    /// 真实微软服务的配置。
    pub fn microsoft(client_id: &str) -> Self {
        Self::with_base(
            "https://login.microsoftonline.com",
            "https://graph.microsoft.com",
            client_id,
        )
    }

    /// 自定义基地址（例如本地模拟服务，二者可以相同）。
    pub fn with_base(authority: &str, graph: &str, client_id: &str) -> Self {
        Self {
            authority: authority.trim_end_matches('/').to_string(),
            tenant: "common".into(),
            graph: graph.trim_end_matches('/').to_string(),
            client_id: client_id.to_string(),
            scopes: ["offline_access", "User.Read", "Mail.ReadWrite", "Mail.Send"]
                .map(String::from)
                .to_vec(),
        }
    }

    fn token_url(&self) -> String {
        format!("{}/{}/oauth2/v2.0/token", self.authority, self.tenant)
    }
}

/// 访问令牌与刷新令牌。刷新令牌需要安全保存（计划存入系统凭据管理器）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// 访问令牌过期的 Unix 时间（秒）。
    pub expires_at: u64,
}

impl Tokens {
    pub fn is_expired(&self) -> bool {
        now_secs() + 60 >= self.expires_at
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// PKCE 的 verifier 与 challenge（S256）。
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        // 两个随机 UUID 拼成 64 个字符的 verifier（RFC 7636 要求 43–128 个字符）。
        let verifier = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// 一次进行中的登录：本机回调监听器 + PKCE + state。
pub struct PendingLogin {
    listener: TcpListener,
    pub redirect_uri: String,
    pub authorize_url: String,
    pkce: Pkce,
    state: String,
}

fn encode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// 开始登录：在回环地址的随机端口上监听，生成授权页地址。
/// 调用方用系统浏览器打开 `authorize_url`，然后调用 [`PendingLogin::finish`]。
pub fn begin_login(config: &OutlookConfig) -> Result<PendingLogin, SyncError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}/");
    let pkce = Pkce::new();
    let state = uuid::Uuid::new_v4().simple().to_string();
    let authorize_url = format!(
        "{}/{}/oauth2/v2.0/authorize?client_id={}&response_type=code&redirect_uri={}&response_mode=query&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        config.authority,
        config.tenant,
        encode(&config.client_id),
        encode(&redirect_uri),
        encode(&config.scopes.join(" ")),
        state,
        pkce.challenge,
    );
    Ok(PendingLogin {
        listener,
        redirect_uri,
        authorize_url,
        pkce,
        state,
    })
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Letterdesk</title><p style=\"font-family:sans-serif\">登录完成，可以关闭这个页面，回到 Letterdesk。</p>";

impl PendingLogin {
    /// 等待浏览器带着授权码跳回（最长 `timeout`），再换取令牌。
    pub fn finish(self, config: &OutlookConfig, timeout: Duration) -> Result<Tokens, SyncError> {
        let code = self.wait_for_code(timeout)?;
        exchange(
            config,
            &[
                ("client_id", config.client_id.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("code_verifier", self.pkce.verifier.as_str()),
                ("scope", config.scopes.join(" ").as_str()),
            ],
        )
    }

    fn wait_for_code(&self, timeout: Duration) -> Result<String, SyncError> {
        self.listener.set_nonblocking(true)?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    let mut reader = BufReader::new(stream.try_clone()?);
                    let mut request_line = String::new();
                    reader.read_line(&mut request_line)?;
                    // 读完请求头。
                    loop {
                        let mut l = String::new();
                        if reader.read_line(&mut l)? == 0 || l == "\r\n" || l == "\n" {
                            break;
                        }
                    }
                    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
                    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
                    let params: std::collections::HashMap<String, String> =
                        form_urlencoded::parse(query.as_bytes())
                            .into_owned()
                            .collect();
                    let mut out = stream;
                    let respond = |out: &mut std::net::TcpStream, body: &str| {
                        let _ = write!(
                            out,
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    };
                    if let Some(error) = params.get("error") {
                        respond(&mut out, "登录未完成，请回到 Letterdesk 重试。");
                        let detail = params.get("error_description").cloned().unwrap_or_default();
                        return Err(SyncError::Auth(format!("{error}: {detail}")));
                    }
                    let Some(code) = params.get("code") else {
                        // 浏览器可能先请求 favicon 等，忽略。
                        respond(&mut out, "");
                        continue;
                    };
                    if params.get("state") != Some(&self.state) {
                        respond(&mut out, "登录状态不匹配，请回到 Letterdesk 重试。");
                        return Err(SyncError::Auth("state 不匹配，可能是伪造的回调".into()));
                    }
                    respond(&mut out, DONE_PAGE);
                    return Ok(code.clone());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(SyncError::Auth("等待浏览器登录超时".into()));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// 用刷新令牌换取新的令牌（刷新令牌只能用一次，需保存新的）。
pub fn refresh(config: &OutlookConfig, tokens: &Tokens) -> Result<Tokens, SyncError> {
    exchange(
        config,
        &[
            ("client_id", config.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", tokens.refresh_token.as_str()),
            ("scope", config.scopes.join(" ").as_str()),
        ],
    )
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

fn exchange(config: &OutlookConfig, form: &[(&str, &str)]) -> Result<Tokens, SyncError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut res = agent
        .post(&config.token_url())
        .send_form(form.iter().copied())
        .map_err(|e| SyncError::Network(e.to_string()))?;
    let status = res.status().as_u16();
    let body: serde_json::Value = res
        .body_mut()
        .read_json()
        .map_err(|e| SyncError::Protocol(format!("令牌响应无法解析：{e}")))?;
    if status != 200 {
        let error = body["error"].as_str().unwrap_or("unknown");
        let detail = body["error_description"].as_str().unwrap_or_default();
        return Err(SyncError::Auth(format!("{error}: {detail}")));
    }
    let t: TokenResponse = serde_json::from_value(body)
        .map_err(|e| SyncError::Protocol(format!("令牌响应缺少字段：{e}")))?;
    Ok(Tokens {
        access_token: t.access_token,
        refresh_token: t.refresh_token,
        expires_at: now_secs() + t.expires_in,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_verifier() {
        let p = Pkce::new();
        assert_eq!(p.verifier.len(), 64);
        assert_eq!(
            p.challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(p.verifier.as_bytes()))
        );
        assert_ne!(Pkce::new().verifier, p.verifier);
    }

    #[test]
    fn authorize_url_has_required_parameters() {
        let config = OutlookConfig::microsoft("my-client");
        let login = begin_login(&config).unwrap();
        let url = &login.authorize_url;
        assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?"));
        for part in [
            "client_id=my-client",
            "response_type=code",
            "code_challenge_method=S256",
            "offline_access",
            "Mail.ReadWrite",
        ] {
            assert!(url.contains(part), "{part} missing in {url}");
        }
        assert!(login.redirect_uri.starts_with("http://localhost:"));
    }
}
