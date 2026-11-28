//! 对着本地模拟服务走完整流程：浏览器登录（PKCE）→ 首次同步 → 无变化 → 新邮件与删除 → 令牌过期后自动刷新。

use mail_store::Store;
mod support;
use mail_sync::maildir::sync_outlook_to_dir;
use mail_sync::oauth::{OutlookConfig, begin_login};
use mail_sync::outlook::OutlookClient;
use mock_servers::{CLIENT_ID, Mailbox, graph};
use std::time::Duration;
use support::read_messages;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

#[test]
fn outlook_login_and_incremental_sync() {
    let mailbox = Mailbox::new("lin@example.com", "Lin Chen");
    let samples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let loaded = mailbox.load_dir(&samples).unwrap();
    let mock = graph::start(mailbox.clone(), "127.0.0.1:0").unwrap();
    let config = OutlookConfig::with_base(&mock.base_url, &mock.base_url, CLIENT_ID);

    // 登录：另一个线程扮演浏览器，打开授权页并跟随跳转回到本机回调。
    let login = begin_login(&config).unwrap();
    let url = login.authorize_url.clone();
    let browser = std::thread::spawn(move || {
        let mut res = agent().get(&url).call().unwrap();
        (
            res.status().as_u16(),
            res.body_mut().read_to_string().unwrap(),
        )
    });
    let tokens = login.finish(&config, Duration::from_secs(10)).unwrap();
    let (status, page) = browser.join().unwrap();
    assert_eq!(status, 200);
    assert!(page.contains("登录完成"));

    let mut client = OutlookClient::new(config, tokens).with_page_size(3);
    assert_eq!(
        client.me().unwrap(),
        ("Lin Chen".to_string(), "lin@example.com".to_string())
    );

    // 首次同步：收件箱和已发送里的全部邮件。
    let dir = tempfile::tempdir().unwrap();
    let stats = sync_outlook_to_dir(&mut client, dir.path()).unwrap();
    assert_eq!(stats.downloaded, loaded);
    assert_eq!(read_messages(dir.path()).len(), loaded);
    let state = Store::open(dir.path())
        .unwrap()
        .begin_folder()
        .unwrap()
        .state;
    assert_eq!(state.delta_links.len(), 3);

    // 下载的邮件可以解析，并能按人归组。
    let emails: Vec<_> = read_messages(dir.path())
        .iter()
        .map(|(_, raw)| mail_core::Email::parse(raw).unwrap())
        .collect();
    assert_eq!(emails.len(), loaded);

    // 没有变化时不下载。
    let dir = support::legacy_copy(dir.path(), &state);
    assert_eq!(
        sync_outlook_to_dir(&mut client, dir.path())
            .unwrap()
            .downloaded,
        0
    );

    // 来了一封新邮件，删了一封旧邮件。
    let a = agent();
    a.post(&format!("{}/mock/deliver", mock.base_url))
        .send(&b"From: carol@example.com\r\nTo: lin@example.com\r\nSubject: Hello again\r\nDate: Sun, 15 Mar 2026 09:00:00 +0000\r\nMessage-ID: <again@x>\r\n\r\nhi"[..])
        .unwrap();
    a.post(&format!("{}/mock/delete/AAMkAG00000001", mock.base_url))
        .send_empty()
        .unwrap();
    let stats = sync_outlook_to_dir(&mut client, dir.path()).unwrap();
    assert_eq!((stats.downloaded, stats.removed), (1, 1));
    assert_eq!(read_messages(dir.path()).len(), loaded);
    assert!(
        read_messages(dir.path())
            .iter()
            .all(|(key, _)| key != "AAMkAG00000001.eml")
    );

    // 访问令牌失效：客户端用刷新令牌换新令牌后继续同步。
    let before = client.tokens().clone();
    a.post(&format!("{}/mock/expire-tokens", mock.base_url))
        .send_empty()
        .unwrap();
    a.post(&format!("{}/mock/deliver", mock.base_url))
        .send(&b"From: dan@example.com\r\nTo: lin@example.com\r\nSubject: After refresh\r\nMessage-ID: <after@x>\r\n\r\nhi"[..])
        .unwrap();
    let stats = sync_outlook_to_dir(&mut client, dir.path()).unwrap();
    assert_eq!(stats.downloaded, 1);
    assert_ne!(client.tokens().access_token, before.access_token);
    assert_ne!(client.tokens().refresh_token, before.refresh_token);
}

#[test]
fn forged_callback_state_is_rejected() {
    let mock = graph::start(Mailbox::new("lin@example.com", "Lin"), "127.0.0.1:0").unwrap();
    let config = OutlookConfig::with_base(&mock.base_url, &mock.base_url, CLIENT_ID);
    let login = begin_login(&config).unwrap();
    let callback = format!("{}?code=stolen&state=forged", login.redirect_uri);
    let attacker = std::thread::spawn(move || {
        let _ = agent().get(&callback).call();
    });
    let err = login.finish(&config, Duration::from_secs(10)).unwrap_err();
    attacker.join().unwrap();
    assert!(err.to_string().contains("state"), "{err}");
}
