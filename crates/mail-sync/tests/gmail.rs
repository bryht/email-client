//! 对着本地模拟的 Gmail IMAP / SMTP 走完整流程。

use mail_store::Store;
mod support;
use mail_sync::SyncError;
use mail_sync::gmail::{GmailConfig, send_gmail, sync_gmail_to_dir};
use mock_servers::{APP_PASSWORD, Folder, Mailbox, imap, smtp};
use support::read_messages;

fn setup() -> (Mailbox, imap::ImapMock, smtp::SmtpMock, usize) {
    let mailbox = Mailbox::new("lin@example.com", "Lin Chen");
    let samples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let n = mailbox.load_dir(&samples).unwrap();
    let imap = imap::start(mailbox.clone(), "127.0.0.1:0").unwrap();
    let smtp = smtp::start(mailbox.clone(), "127.0.0.1:0").unwrap();
    (mailbox, imap, smtp, n)
}

#[test]
fn gmail_incremental_sync_and_send() {
    let (mailbox, imap, smtp, loaded) = setup();
    let config = GmailConfig::plain(
        "127.0.0.1",
        imap.port,
        smtp.port,
        "lin@example.com",
        APP_PASSWORD,
    );
    let dir = tempfile::tempdir().unwrap();

    // 首次同步：收件箱 + 已发送里的全部样例，文件按 X-GM-MSGID 命名。
    let stats = sync_gmail_to_dir(&config, dir.path()).unwrap();
    assert_eq!(stats.downloaded, loaded);
    let files = read_messages(dir.path());
    assert_eq!(files.len(), loaded);
    assert!(
        files.iter().all(|(key, _)| key.starts_with("gm-")),
        "{files:?}"
    );
    let state = Store::open(dir.path())
        .unwrap()
        .begin_folder()
        .unwrap()
        .state;
    assert!(state.imap.contains_key("INBOX"));
    assert!(state.imap.contains_key("[Gmail]/Sent Mail"));
    assert!(state.imap.contains_key("[Gmail]/Drafts"));
    for (_, raw) in &files {
        mail_core::Email::parse(raw).unwrap();
    }

    // 没有变化时不下载。
    let dir = support::legacy_copy(dir.path(), &state);
    assert_eq!(
        sync_gmail_to_dir(&config, dir.path()).unwrap().downloaded,
        0
    );

    // 新邮件 + 服务器上删除一封。
    mailbox.deliver(b"From: carol@example.com\r\nTo: lin@example.com\r\nSubject: Ping\r\nDate: Sun, 15 Mar 2026 09:00:00 +0000\r\nMessage-ID: <ping@x>\r\n\r\nhi".to_vec());
    let victim = mailbox
        .snapshot()
        .0
        .iter()
        .find(|m| m.folder == Folder::Inbox)
        .unwrap()
        .n;
    mailbox.delete(victim);
    let stats = sync_gmail_to_dir(&config, dir.path()).unwrap();
    assert_eq!((stats.downloaded, stats.removed), (1, 1));
    assert_eq!(read_messages(dir.path()).len(), loaded);

    // 通过 SMTP 发信：信封收件人含 Bcc；Gmail 自动存入“已发送”，下一次同步取回。
    let raw = b"From: Lin Chen <lin@example.com>\r\nTo: carol@example.com\r\nSubject: Re: Ping\r\nMessage-ID: <reply@x>\r\nIn-Reply-To: <ping@x>\r\n\r\nPong.\r\n";
    send_gmail(
        &config,
        "lin@example.com",
        &["carol@example.com".into(), "audit@example.com".into()],
        raw,
    )
    .unwrap();
    let sent = smtp.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].recipients,
        vec!["carol@example.com", "audit@example.com"]
    );
    assert_eq!(
        sync_gmail_to_dir(&config, dir.path()).unwrap().downloaded,
        1
    );
    let found = read_messages(dir.path()).iter().any(|(_, raw)| {
        let e = mail_core::Email::parse(raw).unwrap();
        e.subject == "Re: Ping" && e.in_reply_to == vec!["ping@x".to_string()]
    });
    assert!(found);
}

#[test]
fn since_window_limits_first_sync() {
    let (mailbox, imap, smtp, _) = setup();
    let mut config = GmailConfig::plain(
        "127.0.0.1",
        imap.port,
        smtp.port,
        "lin@example.com",
        APP_PASSWORD,
    );
    config.since_days = Some(30);
    // 样例都是 2026 年 3 月的；只有今天投递的这封在 30 天之内。
    let now = time::OffsetDateTime::now_utc();
    let short = |s: String| s.chars().take(3).collect::<String>();
    let today = format!(
        "{}, {:02} {} {} 12:00:00 +0000",
        short(now.weekday().to_string()),
        now.day(),
        short(now.month().to_string()),
        now.year()
    );
    mailbox.deliver(format!("From: dan@example.com\r\nTo: lin@example.com\r\nSubject: Recent\r\nDate: {today}\r\n\r\nnew").into_bytes());
    let dir = tempfile::tempdir().unwrap();
    let stats = sync_gmail_to_dir(&config, dir.path()).unwrap();
    assert_eq!(stats.downloaded, 1);
}

#[test]
fn wrong_app_password_is_an_auth_error() {
    let (_, imap, smtp, _) = setup();
    let config = GmailConfig::plain(
        "127.0.0.1",
        imap.port,
        smtp.port,
        "lin@example.com",
        "wrong pass word xxxx",
    );
    let dir = tempfile::tempdir().unwrap();
    let err = sync_gmail_to_dir(&config, dir.path()).unwrap_err();
    assert!(matches!(err, SyncError::Auth(_)), "{err}");
    let err = send_gmail(
        &config,
        "lin@example.com",
        &["a@example.com".into()],
        b"Subject: x\r\n\r\ny",
    )
    .unwrap_err();
    assert!(matches!(err, SyncError::Auth(_)), "{err}");
}
