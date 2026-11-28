//! 用真实的 HTTP 请求走一遍：PKCE 登录 → /me → 分页增量同步 → 新邮件与删除 → 下载原文 → 发信 → 刷新令牌。

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mock_servers::{CLIENT_ID, Mailbox, graph};
use serde_json::Value;
use sha2::{Digest, Sha256};

const REDIRECT: &str = "http://localhost:53682/callback";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

fn mailbox() -> Mailbox {
    let mb = Mailbox::new("lin@example.com", "Lin Chen");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    mb.load_dir(&dir).unwrap();
    mb
}

/// 完成授权码 + PKCE 流程，返回 (access_token, refresh_token)。
fn login(agent: &ureq::Agent, base: &str, verifier: &str) -> (String, String) {
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let url = format!(
        "{base}/common/oauth2/v2.0/authorize?client_id={CLIENT_ID}&response_type=code&redirect_uri={}&scope=offline_access%20Mail.ReadWrite%20Mail.Send&state=xyz&code_challenge={challenge}&code_challenge_method=S256",
        urlencode(REDIRECT)
    );
    let res = agent.get(&url).call().unwrap();
    assert_eq!(res.status(), 302);
    let location = res.headers()["location"].to_str().unwrap().to_string();
    assert!(location.starts_with(REDIRECT), "{location}");
    assert!(location.ends_with("&state=xyz"));
    let code = location
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    let mut res = agent
        .post(&format!("{base}/common/oauth2/v2.0/token"))
        .send_form([
            ("client_id", CLIENT_ID),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", verifier),
        ])
        .unwrap();
    assert_eq!(res.status(), 200);
    let tokens: Value = res.body_mut().read_json().unwrap();
    assert_eq!(tokens["token_type"], "Bearer");
    (
        tokens["access_token"].as_str().unwrap().to_string(),
        tokens["refresh_token"].as_str().unwrap().to_string(),
    )
}

fn urlencode(s: &str) -> String {
    form_urlencoded_encode(s)
}

fn form_urlencoded_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn get_json(agent: &ureq::Agent, url: &str, token: &str, page: Option<usize>) -> (u16, Value) {
    let mut req = agent
        .get(url)
        .header("Authorization", &format!("Bearer {token}"));
    if let Some(n) = page {
        req = req.header("Prefer", &format!("odata.maxpagesize={n}"));
    }
    let mut res = req.call().unwrap();
    let status = res.status().as_u16();
    (status, res.body_mut().read_json().unwrap_or(Value::Null))
}

/// 跟随 nextLink 直到拿到 deltaLink，返回 (所有条目, deltaLink, 页数)。
fn sync_folder(agent: &ureq::Agent, start: &str, token: &str) -> (Vec<Value>, String, usize) {
    let mut url = start.to_string();
    let mut items = Vec::new();
    let mut pages = 0;
    loop {
        let (status, body) = get_json(agent, &url, token, Some(3));
        assert_eq!(status, 200, "{body}");
        pages += 1;
        items.extend(body["value"].as_array().unwrap().iter().cloned());
        if let Some(next) = body["@odata.nextLink"].as_str() {
            url = next.to_string();
        } else {
            return (
                items,
                body["@odata.deltaLink"].as_str().unwrap().to_string(),
                pages,
            );
        }
    }
}

#[test]
fn pkce_login_and_delta_sync() {
    let mock = graph::start(mailbox(), "127.0.0.1:0").unwrap();
    let base = &mock.base_url;
    let agent = agent();
    let (access, refresh) = login(
        &agent,
        base,
        "a-long-random-code-verifier-0123456789abcdefghijkl",
    );

    let (status, me) = get_json(&agent, &format!("{base}/v1.0/me"), &access, None);
    assert_eq!(status, 200);
    assert_eq!(me["mail"], "lin@example.com");

    // 首次同步：收件箱的样例邮件分多页返回。
    let start = format!("{base}/v1.0/me/mailFolders/inbox/messages/delta");
    let (items, delta_link, pages) = sync_folder(&agent, &start, &access);
    assert!(items.len() >= 8, "inbox has {} messages", items.len());
    assert!(pages >= 3);
    let first = &items[0];
    assert!(first["id"].as_str().unwrap().starts_with("AAMkAG"));
    assert!(first["from"]["emailAddress"]["address"].is_string());
    assert!(first["receivedDateTime"].as_str().unwrap().ends_with('Z'));

    // 下载原文并能解析。
    let id = first["id"].as_str().unwrap();
    let mut res = agent
        .get(&format!("{base}/v1.0/me/messages/{id}/$value"))
        .header("Authorization", &format!("Bearer {access}"))
        .call()
        .unwrap();
    let raw = res.body_mut().read_to_vec().unwrap();
    assert!(mail_core::Email::parse(&raw).is_ok());

    // 没有变化时增量同步为空。
    let (items, delta_link2, _) = sync_folder(&agent, &delta_link, &access);
    assert!(items.is_empty());

    // 投递一封新邮件、删除一封旧邮件，增量同步只返回这两项变化。
    agent
        .post(&format!("{base}/mock/deliver"))
        .send(&b"From: carol@example.com\r\nTo: lin@example.com\r\nSubject: New\r\nDate: Sat, 14 Mar 2026 10:00:00 +0000\r\nMessage-ID: <new@x>\r\n\r\nhello"[..])
        .unwrap();
    agent
        .post(&format!("{base}/mock/delete/{id}"))
        .send_empty()
        .unwrap();
    let (items, _, _) = sync_folder(&agent, &delta_link2, &access);
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(
        items
            .iter()
            .any(|i| i["subject"] == "New" && i["internetMessageId"] == "<new@x>")
    );
    assert!(
        items
            .iter()
            .any(|i| i["id"] == id && i["@removed"]["reason"] == "deleted")
    );

    // 标记已读。
    let new_id = items.iter().find(|i| i["subject"] == "New").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = agent
        .patch(&format!("{base}/v1.0/me/messages/{new_id}"))
        .header("Authorization", &format!("Bearer {access}"))
        .send_json(serde_json::json!({"isRead": true}))
        .unwrap();
    assert_eq!(res.status(), 200);

    // 发信后出现在“已发送”。
    let res = agent
        .post(&format!("{base}/v1.0/me/sendMail"))
        .header("Authorization", &format!("Bearer {access}"))
        .send_json(serde_json::json!({
            "message": {
                "subject": "Re: New",
                "body": {"contentType": "Text", "content": "Thanks!"},
                "toRecipients": [{"emailAddress": {"address": "carol@example.com"}}]
            },
            "saveToSentItems": true
        }))
        .unwrap();
    assert_eq!(res.status(), 202);
    let (sent, _, _) = sync_folder(
        &agent,
        &format!("{base}/v1.0/me/mailFolders/sentitems/messages/delta"),
        &access,
    );
    assert!(sent.iter().any(|m| m["subject"] == "Re: New"));

    // 令牌失效后返回 401，用刷新令牌换新令牌后恢复。
    agent
        .post(&format!("{base}/mock/expire-tokens"))
        .send_empty()
        .unwrap();
    let (status, body) = get_json(&agent, &format!("{base}/v1.0/me"), &access, None);
    assert_eq!(status, 401);
    assert_eq!(body["error"]["code"], "InvalidAuthenticationToken");
    let mut res = agent
        .post(&format!("{base}/common/oauth2/v2.0/token"))
        .send_form([
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
        ])
        .unwrap();
    let tokens: Value = res.body_mut().read_json().unwrap();
    let access2 = tokens["access_token"].as_str().unwrap();
    assert_eq!(
        get_json(&agent, &format!("{base}/v1.0/me"), access2, None).0,
        200
    );
    // 刷新令牌只能用一次。
    let res = agent
        .post(&format!("{base}/common/oauth2/v2.0/token"))
        .send_form([
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
        ])
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[test]
fn rejects_bad_pkce_and_unknown_client() {
    let mock = graph::start(mailbox(), "127.0.0.1:0").unwrap();
    let base = &mock.base_url;
    let agent = agent();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(b"right-verifier"));
    let url = format!(
        "{base}/common/oauth2/v2.0/authorize?client_id={CLIENT_ID}&response_type=code&redirect_uri={}&scope=Mail.Read&code_challenge={challenge}&code_challenge_method=S256",
        urlencode(REDIRECT)
    );
    let res = agent.get(&url).call().unwrap();
    let location = res.headers()["location"].to_str().unwrap().to_string();
    let code = location.split("code=").nth(1).unwrap();
    let mut res = agent
        .post(&format!("{base}/common/oauth2/v2.0/token"))
        .send_form([
            ("client_id", CLIENT_ID),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", "wrong-verifier"),
        ])
        .unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.body_mut().read_json().unwrap();
    assert_eq!(body["error"], "invalid_grant");

    let res = agent
        .get(&format!("{base}/common/oauth2/v2.0/authorize?client_id=someone-else&response_type=code&redirect_uri={}&code_challenge=x&code_challenge_method=S256", urlencode(REDIRECT)))
        .call()
        .unwrap();
    assert_eq!(res.status(), 400);
    // 没有令牌访问 Graph。
    assert_eq!(
        get_json(&agent, &format!("{base}/v1.0/me"), "nope", None).0,
        401
    );
}
