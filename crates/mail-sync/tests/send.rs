//! 回信与“发送中断”核对：对着本地模拟的 Outlook（Graph）和 Gmail（SMTP + IMAP）。

use mail_core::{Email, Mailbox as Addr};
use mail_sync::compose::ReplyDraft;
use mail_sync::gmail::{GmailConfig, reply_gmail};
use mail_sync::oauth::{OutlookConfig, begin_login};
use mail_sync::outlook::OutlookClient;
use mail_sync::send::SendStatus;
use mock_servers::{APP_PASSWORD, CLIENT_ID, Folder, Mailbox, graph, imap, smtp};
use std::time::Duration;

fn mailbox() -> Mailbox {
    let mb = Mailbox::new("lin@example.com", "Lin Chen");
    let samples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    mb.load_dir(&samples).unwrap();
    mb
}

fn draft() -> ReplyDraft {
    ReplyDraft {
        to: vec![Addr::new(Some("Maya Patel"), "maya@example.com")],
        cc: vec![Addr::new(Some("Alex Carter"), "alex@example.com")],
        text: "Hi Maya,\n\nThursday works.\n\nBest,\nLin".into(),
        html: "<p>Hi Maya,</p><p>Thursday works.</p><p>Best,<br>Lin</p>".into(),
    }
}

/// 找到样例里 Maya 的会议邮件。
fn maya(mb: &Mailbox) -> (u64, Email) {
    let (msgs, _) = mb.snapshot();
    let m = msgs
        .iter()
        .find(|m| m.email.subject == "Re: Pilot meeting")
        .unwrap();
    (m.n, m.email.clone())
}

fn sent_with_id(mb: &Mailbox, message_id: &str) -> Option<Email> {
    mb.snapshot()
        .0
        .into_iter()
        .find(|m| {
            m.folder == Folder::Sent
                && !m.deleted
                && m.email.message_id.as_deref() == Some(message_id)
        })
        .map(|m| m.email)
}

fn outlook_client(base: &str) -> OutlookClient {
    let config = OutlookConfig::with_base(base, base, CLIENT_ID);
    let login = begin_login(&config).unwrap();
    let url = login.authorize_url.clone();
    std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let _ = agent.get(&url).call();
    });
    let tokens = login.finish(&config, Duration::from_secs(10)).unwrap();
    OutlookClient::new(config, tokens)
}

#[test]
fn outlook_reply_all_and_interrupted_send() {
    let mb = mailbox();
    let mock = graph::start(mb.clone(), "127.0.0.1:0").unwrap();
    let mut client = outlook_client(&mock.base_url);
    let (n, _) = maya(&mb);
    let original_id = format!("AAMkAG{n:08}");

    // 正常发送：服务器上的邮件带线程信息、用户确认的收件人与正文。
    let report = client.reply_all(&original_id, &draft()).unwrap();
    assert_eq!(report.status, SendStatus::Sent);
    let sent = sent_with_id(&mb, &report.message_id).expect("in sent items");
    assert_eq!(sent.subject, "Re: Pilot meeting");
    assert_eq!(sent.in_reply_to, vec!["maya-2@example.com"]);
    assert_eq!(
        sent.references,
        vec!["maya-1@example.com", "maya-2@example.com"]
    );
    assert_eq!(sent.to[0].address, "maya@example.com");
    assert_eq!(sent.cc[0].address, "alex@example.com");
    assert!(sent.html.contains("Thursday works."));
    // 草稿已不在“草稿”里。
    assert!(
        !mb.snapshot()
            .0
            .iter()
            .any(|m| m.folder == Folder::Drafts && !m.deleted)
    );

    // 服务器发出了但返回 503：到“已发送”核对后确认已发送。
    ureq::post(&format!("{}/mock/drop-next-send", mock.base_url))
        .send_empty()
        .unwrap();
    let report = client.reply_all(&original_id, &draft()).unwrap();
    assert_eq!(report.status, SendStatus::SentConfirmedAfterError);
    assert!(report.status.is_sent());

    // 服务器拒绝（不是草稿）：未发送。
    let status = client.send_draft(&original_id, "nope@x");
    assert!(matches!(status, SendStatus::NotSent(_)), "{status:?}");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn gmail_reply_and_interrupted_send() {
    let mb = mailbox();
    let imap = imap::start(mb.clone(), "127.0.0.1:0").unwrap();
    let smtp = smtp::start(mb.clone(), "127.0.0.1:0").unwrap();
    let config = GmailConfig::plain(
        "127.0.0.1",
        imap.port,
        smtp.port,
        "lin@example.com",
        APP_PASSWORD,
    );
    let (_, original) = maya(&mb);
    let me = Addr::new(Some("Lin Chen"), "lin@example.com");

    let report = reply_gmail(&config, &original, &me, &draft()).unwrap();
    assert_eq!(report.status, SendStatus::Sent);
    let sent = sent_with_id(&mb, &report.message_id).expect("gmail saved it to Sent");
    assert_eq!(sent.in_reply_to, vec!["maya-2@example.com"]);
    assert_eq!(
        smtp.sent().last().unwrap().recipients,
        vec!["maya@example.com", "alex@example.com"]
    );

    // 服务器收下了但没回 250 就断开：核对后确认已发送。
    smtp.drop_next_after_data();
    let report = reply_gmail(&config, &original, &me, &draft()).unwrap();
    assert_eq!(report.status, SendStatus::SentConfirmedAfterError);

    // SMTP 连不上：核对“已发送”没有找到 → 未发送，可以重试。
    let mut down = config.clone();
    down.smtp_port = free_port();
    let report = reply_gmail(&down, &original, &me, &draft()).unwrap();
    assert!(
        matches!(report.status, SendStatus::NotSent(_)),
        "{:?}",
        report.status
    );

    // SMTP 和 IMAP 都连不上：无法核对 → 可能已发送，由用户决定。
    down.imap_port = free_port();
    let report = reply_gmail(&down, &original, &me, &draft()).unwrap();
    assert!(
        matches!(report.status, SendStatus::MaybeSent(_)),
        "{:?}",
        report.status
    );
}
