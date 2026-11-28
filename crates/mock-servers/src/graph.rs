//! 模拟微软身份平台（OAuth 2.0 授权码 + PKCE）与 Microsoft Graph 邮件接口。
//!
//! 路径与真实服务一致，客户端只需把基地址从
//! `https://login.microsoftonline.com` / `https://graph.microsoft.com` 换成本服务的地址：
//!
//! - `GET  /{tenant}/oauth2/v2.0/authorize`：自动同意，302 跳回 `redirect_uri?code=…&state=…`
//! - `POST /{tenant}/oauth2/v2.0/token`：`authorization_code`（校验 PKCE）与 `refresh_token`
//! - `GET  /v1.0/me`
//! - `GET  /v1.0/me/mailFolders/{inbox|sentitems|drafts}/messages/delta`：分页
//!   （`Prefer: odata.maxpagesize=N`、`@odata.nextLink`），最后一页给 `@odata.deltaLink`；
//!   带 `$deltatoken` 时只返回变化，删除的邮件以 `@removed` 表示
//! - `GET  /v1.0/me/messages/{id}`、`GET /v1.0/me/messages/{id}/$value`（MIME 原文）
//! - `PATCH /v1.0/me/messages/{id}`：`{"isRead": bool}`
//! - `POST /v1.0/me/sendMail`：发信并存入“已发送”
//!
//! 测试用的管理接口：`POST /mock/deliver`（请求体为一封 `.eml`，投递到收件箱）、
//! `POST /mock/delete/{id}`、`POST /mock/expire-tokens`（让所有访问令牌失效）。

use crate::store::{Folder, Mailbox, StoredMessage, iso8601};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tiny_http::{Header, Method, Request, Response, Server};

/// 模拟服务接受的应用（客户端）ID。
pub const CLIENT_ID: &str = "00000000-1e77-4de5-8000-000000000001";

const DEFAULT_PAGE_SIZE: usize = 10;

struct CodeGrant {
    redirect_uri: String,
    challenge: String,
    scope: String,
}

#[derive(Default)]
struct AuthState {
    codes: HashMap<String, CodeGrant>,
    access: HashSet<String>,
    refresh: HashMap<String, String>,
}

/// 运行中的模拟服务。丢弃时停止。
pub struct GraphMock {
    /// 例如 `http://127.0.0.1:8901`，同时作为登录与 Graph 的基地址。
    pub base_url: String,
    server: Arc<Server>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for GraphMock {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 在 `addr`（如 `127.0.0.1:0`）上启动模拟服务。
pub fn start(mailbox: Mailbox, addr: &str) -> std::io::Result<GraphMock> {
    let server = Arc::new(Server::http(addr).map_err(std::io::Error::other)?);
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or_default();
    let base_url = format!("http://127.0.0.1:{port}");
    let auth = Arc::new(Mutex::new(AuthState::default()));
    let srv = server.clone();
    let base = base_url.clone();
    let thread = std::thread::spawn(move || {
        while let Ok(request) = srv.recv() {
            handle(request, &mailbox, &auth, &base);
        }
    });
    Ok(GraphMock {
        base_url,
        server,
        thread: Some(thread),
    })
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn json_response(status: u16, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(body.to_string().into_bytes())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
}

fn graph_error(status: u16, code: &str, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(
        status,
        &json!({ "error": { "code": code, "message": message } }),
    )
}

fn oauth_error(error: &str, description: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(
        400,
        &json!({ "error": error, "error_description": description }),
    )
}

fn split_url(url: &str) -> (String, HashMap<String, String>) {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let params = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    (path.to_string(), params)
}

fn read_body(request: &mut Request) -> Vec<u8> {
    let mut body = Vec::new();
    let _ = request.as_reader().read_to_end(&mut body);
    body
}

fn bearer(request: &Request) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| h.value.as_str().strip_prefix("Bearer ").map(String::from))
}

fn page_size(request: &Request) -> usize {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Prefer"))
        .and_then(|h| {
            h.value
                .as_str()
                .split(',')
                .find_map(|p| p.trim().strip_prefix("odata.maxpagesize="))
                .and_then(|n| n.trim().parse().ok())
        })
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_PAGE_SIZE)
}

fn token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn recipients(list: &[mail_core::Mailbox]) -> Value {
    Value::Array(list.iter().map(address).collect())
}

fn address(m: &mail_core::Mailbox) -> Value {
    json!({ "emailAddress": { "name": m.display_name(), "address": m.address } })
}

/// 邮件的 Graph JSON 表示（常用字段）。
pub fn message_json(m: &StoredMessage) -> Value {
    let e = &m.email;
    let ts = e.timestamp.unwrap_or(0);
    let preview: String = e.body.chars().take(255).collect();
    json!({
        "id": m.graph_id(),
        "internetMessageId": e.message_id.as_ref().map(|id| format!("<{id}>")),
        "conversationId": format!("AAQkAD{:08}", m.n),
        "subject": e.subject,
        "from": address(&e.from),
        "sender": address(&e.from),
        "toRecipients": recipients(&e.to),
        "ccRecipients": recipients(&e.cc),
        "replyTo": recipients(&e.reply_to),
        "receivedDateTime": iso8601(ts),
        "sentDateTime": iso8601(ts),
        "isRead": m.seen,
        "isDraft": m.folder == Folder::Drafts,
        "parentFolderId": m.folder.graph_name(),
        "bodyPreview": preview,
        "hasAttachments": false,
    })
}

fn handle(mut request: Request, mailbox: &Mailbox, auth: &Mutex<AuthState>, base: &str) {
    let (path, params) = split_url(request.url());
    let method = request.method().clone();
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

    // ---- 登录 ----
    if segments.len() == 4 && segments[1] == "oauth2" && segments[2] == "v2.0" {
        let response = match (method, segments[3]) {
            (Method::Get, "authorize") => authorize(&params, auth),
            (Method::Post, "token") => {
                let body = read_body(&mut request);
                let form: HashMap<String, String> =
                    form_urlencoded::parse(&body).into_owned().collect();
                token_endpoint(&form, auth)
            }
            _ => graph_error(404, "NotFound", "unknown endpoint"),
        };
        let _ = request.respond(response);
        return;
    }

    // ---- 测试管理接口 ----
    if segments.first() == Some(&"mock") {
        let response = match (method, &segments[1..]) {
            (Method::Post, ["deliver"]) => {
                let body = read_body(&mut request);
                match mailbox.deliver(body) {
                    Some(n) => json_response(201, &json!({ "n": n })),
                    None => graph_error(400, "BadRequest", "unparseable message"),
                }
            }
            (Method::Post, ["delete", id]) => match parse_id(id) {
                Some(n) if mailbox.delete(n) => json_response(200, &json!({})),
                _ => graph_error(404, "ErrorItemNotFound", "not found"),
            },
            (Method::Post, ["expire-tokens"]) => {
                auth.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .access
                    .clear();
                json_response(200, &json!({}))
            }
            _ => graph_error(404, "NotFound", "unknown endpoint"),
        };
        let _ = request.respond(response);
        return;
    }

    // ---- Graph：需要有效的访问令牌 ----
    let authorized = bearer(&request).is_some_and(|t| {
        auth.lock()
            .unwrap_or_else(|e| e.into_inner())
            .access
            .contains(&t)
    });
    if !authorized {
        let _ = request.respond(graph_error(
            401,
            "InvalidAuthenticationToken",
            "Access token is empty, invalid or expired.",
        ));
        return;
    }

    let response = match (method, segments.as_slice()) {
        (Method::Get, ["v1.0", "me"]) => {
            let (owner, name) = mailbox.owner();
            json_response(
                200,
                &json!({
                    "id": "mock-user-1",
                    "displayName": name,
                    "mail": owner,
                    "userPrincipalName": owner,
                }),
            )
        }
        (Method::Get, ["v1.0", "me", "mailFolders", folder, "messages", "delta"]) => {
            match Folder::from_graph_name(folder) {
                Some(f) => delta(mailbox, f, folder, &params, page_size(&request), base),
                None => graph_error(404, "ErrorItemNotFound", "folder not found"),
            }
        }
        (Method::Get, ["v1.0", "me", "messages", id]) => {
            with_message(mailbox, id, |m| json_response(200, &message_json(m)))
        }
        (Method::Get, ["v1.0", "me", "messages", id, "$value"]) => with_message(mailbox, id, |m| {
            Response::from_data(m.raw.clone())
                .with_status_code(200)
                .with_header(header("Content-Type", "text/plain"))
        }),
        (Method::Patch, ["v1.0", "me", "messages", id]) => {
            let body: Value = serde_json::from_slice(&read_body(&mut request)).unwrap_or_default();
            match (parse_id(id), body.get("isRead").and_then(Value::as_bool)) {
                (Some(n), Some(read)) if mailbox.set_seen(n, read) => {
                    with_message(mailbox, id, |m| json_response(200, &message_json(m)))
                }
                (Some(_), None) => graph_error(400, "BadRequest", "only isRead is supported"),
                _ => graph_error(404, "ErrorItemNotFound", "not found"),
            }
        }
        (Method::Post, ["v1.0", "me", "sendMail"]) => {
            let body: Value = serde_json::from_slice(&read_body(&mut request)).unwrap_or_default();
            match build_mime(&body["message"], &mailbox.owner()) {
                Some(raw) => {
                    if body.get("saveToSentItems").and_then(Value::as_bool) != Some(false) {
                        mailbox.record_sent(raw);
                    }
                    Response::from_data(Vec::new()).with_status_code(202)
                }
                None => graph_error(400, "ErrorInvalidRecipients", "message needs toRecipients"),
            }
        }
        _ => graph_error(404, "NotFound", "unknown endpoint"),
    };
    let _ = request.respond(response);
}

fn parse_id(id: &str) -> Option<u64> {
    id.strip_prefix("AAMkAG").and_then(|n| n.parse().ok())
}

fn with_message(
    mailbox: &Mailbox,
    id: &str,
    f: impl FnOnce(&StoredMessage) -> Response<std::io::Cursor<Vec<u8>>>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let (messages, _) = mailbox.snapshot();
    match parse_id(id).and_then(|n| messages.iter().find(|m| m.n == n && !m.deleted)) {
        Some(m) => f(m),
        None => graph_error(
            404,
            "ErrorItemNotFound",
            "The specified object was not found in the store.",
        ),
    }
}

fn authorize(
    params: &HashMap<String, String>,
    auth: &Mutex<AuthState>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let get = |k: &str| params.get(k).map(String::as_str).unwrap_or_default();
    let redirect_uri = get("redirect_uri");
    let fail = |msg: &str| {
        Response::from_data(msg.as_bytes().to_vec())
            .with_status_code(400)
            .with_header(header("Content-Type", "text/plain; charset=utf-8"))
    };
    if get("client_id") != CLIENT_ID {
        return fail("AADSTS700016: Application not found (unknown client_id)");
    }
    if get("response_type") != "code" {
        return fail("AADSTS900144: response_type must be code");
    }
    if get("code_challenge").is_empty() || get("code_challenge_method") != "S256" {
        return fail("AADSTS9002325: PKCE with S256 is required for public clients");
    }
    if !(redirect_uri.starts_with("http://localhost")
        || redirect_uri.starts_with("http://127.0.0.1"))
    {
        return fail("AADSTS50011: redirect_uri must be a loopback address");
    }
    let code = token();
    auth.lock().unwrap_or_else(|e| e.into_inner()).codes.insert(
        code.clone(),
        CodeGrant {
            redirect_uri: redirect_uri.to_string(),
            challenge: get("code_challenge").to_string(),
            scope: get("scope").to_string(),
        },
    );
    let mut location = format!("{redirect_uri}?code={code}");
    if !get("state").is_empty() {
        location.push_str("&state=");
        location.extend(form_urlencoded::byte_serialize(get("state").as_bytes()));
    }
    Response::from_data(Vec::new())
        .with_status_code(302)
        .with_header(header("Location", &location))
}

fn token_endpoint(
    form: &HashMap<String, String>,
    auth: &Mutex<AuthState>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let get = |k: &str| form.get(k).map(String::as_str).unwrap_or_default();
    if get("client_id") != CLIENT_ID {
        return oauth_error("invalid_client", "AADSTS700016: unknown client_id");
    }
    let mut state = auth.lock().unwrap_or_else(|e| e.into_inner());
    let scope = match get("grant_type") {
        "authorization_code" => {
            let Some(grant) = state.codes.remove(get("code")) else {
                return oauth_error(
                    "invalid_grant",
                    "AADSTS70008: code is invalid or was already used",
                );
            };
            if grant.redirect_uri != get("redirect_uri") {
                return oauth_error("invalid_grant", "AADSTS50011: redirect_uri mismatch");
            }
            let digest = Sha256::digest(get("code_verifier").as_bytes());
            if URL_SAFE_NO_PAD.encode(digest) != grant.challenge {
                return oauth_error(
                    "invalid_grant",
                    "AADSTS501481: code_verifier does not match code_challenge",
                );
            }
            grant.scope
        }
        "refresh_token" => match state.refresh.remove(get("refresh_token")) {
            Some(scope) => scope,
            None => return oauth_error("invalid_grant", "AADSTS70000: refresh token is invalid"),
        },
        other => {
            return oauth_error(
                "unsupported_grant_type",
                &format!("grant_type {other} is not supported"),
            );
        }
    };
    let access = token();
    let refresh = token();
    state.access.insert(access.clone());
    state.refresh.insert(refresh.clone(), scope.clone());
    json_response(
        200,
        &json!({
            "token_type": "Bearer",
            "scope": scope,
            "expires_in": 3600,
            "access_token": access,
            "refresh_token": refresh,
        }),
    )
}

fn delta(
    mailbox: &Mailbox,
    folder: Folder,
    folder_name: &str,
    params: &HashMap<String, String>,
    page: usize,
    base: &str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let (messages, version) = mailbox.snapshot();
    // $skiptoken = "起始版本.偏移.截止版本"；$deltatoken = 上次同步时的版本。
    let (since, offset, upto) = match (params.get("$skiptoken"), params.get("$deltatoken")) {
        (Some(skip), _) => {
            let parts: Vec<u64> = skip.split('.').filter_map(|p| p.parse().ok()).collect();
            match parts.as_slice() {
                [s, o, u] => (*s, *o as usize, *u),
                _ => return graph_error(400, "BadRequest", "invalid $skiptoken"),
            }
        }
        (None, Some(delta)) => match delta.parse() {
            Ok(v) => (v, 0, version),
            Err(_) => return graph_error(400, "BadRequest", "invalid $deltatoken"),
        },
        (None, None) => (0, 0, version),
    };
    let changed: Vec<&StoredMessage> = messages
        .iter()
        .filter(|m| m.folder == folder && m.version > since && m.version <= upto)
        // 首次同步不报告已删除的邮件。
        .filter(|m| since > 0 || !m.deleted)
        .collect();
    let items: Vec<Value> = changed
        .iter()
        .skip(offset)
        .take(page)
        .map(|m| {
            if m.deleted {
                json!({ "id": m.graph_id(), "@removed": { "reason": "deleted" } })
            } else {
                message_json(m)
            }
        })
        .collect();
    let link = format!("{base}/v1.0/me/mailFolders/{folder_name}/messages/delta");
    let mut body = json!({
        "@odata.context": format!("{base}/v1.0/$metadata#Collection(message)"),
        "value": items,
    });
    if offset + page < changed.len() {
        body["@odata.nextLink"] = json!(format!(
            "{link}?$skiptoken={since}.{}.{upto}",
            offset + page
        ));
    } else {
        body["@odata.deltaLink"] = json!(format!("{link}?$deltatoken={upto}"));
    }
    json_response(200, &body)
}

/// 根据 Graph 的 message JSON 生成 MIME 原文（纯文本或 HTML 正文）。
fn build_mime(message: &Value, owner: &(String, String)) -> Option<Vec<u8>> {
    let addrs = |key: &str| -> Vec<String> {
        message[key]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|r| r["emailAddress"]["address"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let to = addrs("toRecipients");
    if to.is_empty() {
        return None;
    }
    let cc = addrs("ccRecipients");
    let subject = message["subject"].as_str().unwrap_or_default();
    let content_type = match message["body"]["contentType"].as_str() {
        Some(t) if t.eq_ignore_ascii_case("html") => "text/html",
        _ => "text/plain",
    };
    let content = message["body"]["content"].as_str().unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = crate::store::civil(now);
    let mut raw = format!(
        "From: {} <{}>\r\nTo: {}\r\n",
        owner.1,
        owner.0,
        to.join(", ")
    );
    if !cc.is_empty() {
        raw.push_str(&format!("Cc: {}\r\n", cc.join(", ")));
    }
    raw.push_str(&format!(
        "Subject: {subject}\r\nDate: {d:02} {} {y} {h:02}:{mi:02}:{s:02} +0000\r\nMessage-ID: <{}@mock.graph>\r\nMIME-Version: 1.0\r\nContent-Type: {content_type}; charset=utf-8\r\n\r\n{content}\r\n",
        crate::store::MONTHS[(mo - 1) as usize],
        token()
    ));
    Some(raw.into_bytes())
}
