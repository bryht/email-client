use super::*;

const ME: &str = "lin@example.com";

/// 构造一封邮件。`refs` 为 References（最后一项同时作为 In-Reply-To）。
fn mail(
    id: &str,
    ts: i64,
    from: &str,
    to: &[&str],
    cc: &[&str],
    subject: &str,
    refs: &[&str],
) -> Email {
    let mut raw = format!(
        "From: {from}\r\nTo: {}\r\nSubject: {subject}\r\nMessage-ID: <{id}>\r\nDate: {}\r\n",
        to.join(", "),
        rfc2822(ts)
    );
    if !cc.is_empty() {
        raw.push_str(&format!("Cc: {}\r\n", cc.join(", ")));
    }
    if let Some(last) = refs.last() {
        raw.push_str(&format!("In-Reply-To: <{last}>\r\n"));
        let all: Vec<String> = refs.iter().map(|r| format!("<{r}>")).collect();
        raw.push_str(&format!("References: {}\r\n", all.join(" ")));
    }
    raw.push_str("\r\nbody\r\n");
    Email::parse(raw.as_bytes()).unwrap()
}

/// 2026-03-01 00:00:00 UTC 起的第 `day` 天。
fn rfc2822(day: i64) -> String {
    format!("{:02} Mar 2026 09:00:00 +0000", 1 + day)
}

fn conv_of(convs: &[Conversation], email_index: usize) -> &Conversation {
    convs
        .iter()
        .find(|c| c.messages.contains(&email_index))
        .unwrap()
}

fn addrs(c: &Conversation) -> Vec<&str> {
    c.participants.iter().map(|m| m.address.as_str()).collect()
}

#[test]
fn reply_with_new_cc_stays_in_conversation() {
    // 周一 Maya 单独写给我；周二她回复时抄送 Alex。
    let emails = vec![
        mail("m1", 0, "maya@example.com", &[ME], &[], "Pilot", &[]),
        mail(
            "m2",
            1,
            "maya@example.com",
            &[ME],
            &["alex@example.com"],
            "Re: Pilot",
            &["m1"],
        ),
        mail(
            "m3",
            2,
            ME,
            &["maya@example.com"],
            &["alex@example.com"],
            "Re: Pilot",
            &["m1", "m2"],
        ),
    ];
    let convs = group(&emails, &[ME.into()]);
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].messages, vec![0, 1, 2]);
    assert_eq!(addrs(&convs[0]), vec!["maya@example.com"]);
    assert_eq!(convs[0].threads.len(), 1);
    assert_eq!(convs[0].threads[0].subject, "Pilot");
}

#[test]
fn new_message_matches_by_direct_participants_ignoring_cc() {
    // 另写给 Maya 并首次抄送 Alex：仍归入「Maya」，并成为一个新话题。
    let emails = vec![
        mail("m1", 0, "maya@example.com", &[ME], &[], "Pilot", &[]),
        mail(
            "n1",
            3,
            ME,
            &["Maya@Example.com"],
            &["alex@example.com"],
            "Pricing",
            &[],
        ),
    ];
    let convs = group(&emails, &[ME.into()]);
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].threads.len(), 2);
    assert_eq!(convs[0].threads[1].subject, "Pricing");
}

#[test]
fn different_direct_participants_are_different_conversations() {
    let emails = vec![
        mail("a", 0, "maya@example.com", &[ME], &[], "Hi", &[]),
        mail(
            "b",
            1,
            "maya@example.com",
            &[ME, "alex@example.com"],
            &[],
            "Hi both",
            &[],
        ),
        mail("c", 2, "alex@example.com", &[ME], &[], "Hi from Alex", &[]),
    ];
    let convs = group(&emails, &[ME.into()]);
    assert_eq!(convs.len(), 3);
    assert_eq!(
        addrs(conv_of(&convs, 1)),
        vec!["alex@example.com", "maya@example.com"]
    );
}

#[test]
fn forward_starts_new_conversation_and_replies_stay_there() {
    // Maya 写给我；我把它转发给 Carol；Carol 回复时又抄送了 Maya。
    let emails = vec![
        mail("m1", 0, "maya@example.com", &[ME], &[], "Contract", &[]),
        mail(
            "f1",
            1,
            ME,
            &["carol@example.com"],
            &[],
            "Fwd: Contract",
            &[],
        ),
        mail(
            "r1",
            2,
            "carol@example.com",
            &[ME],
            &["maya@example.com"],
            "Re: Fwd: Contract",
            &["f1"],
        ),
    ];
    let convs = group(&emails, &[ME.into()]);
    assert_eq!(convs.len(), 2);
    let original = conv_of(&convs, 0);
    let forwarded = conv_of(&convs, 1);
    assert_ne!(original.id, forwarded.id);
    assert_eq!(forwarded.messages, vec![1, 2]);
    assert_eq!(addrs(forwarded), vec!["carol@example.com"]);
    assert_eq!(forwarded.threads[0].subject, "Contract");
}

#[test]
fn forward_with_reference_records_source() {
    let emails = vec![
        mail("m1", 0, "maya@example.com", &[ME], &[], "Contract", &[]),
        mail(
            "f1",
            1,
            ME,
            &["carol@example.com"],
            &[],
            "FW: Contract",
            &["m1"],
        ),
    ];
    let convs = group(&emails, &[ME.into()]);
    let original = conv_of(&convs, 0).id;
    assert_eq!(conv_of(&convs, 1).forwarded_from, Some(original));
}

#[test]
fn only_cc_me_groups_by_sender_and_to() {
    let emails = vec![mail(
        "x",
        0,
        "maya@example.com",
        &["bob@example.com"],
        &[ME],
        "FYI",
        &[],
    )];
    let convs = group(&emails, &[ME.into()]);
    assert_eq!(
        addrs(&convs[0]),
        vec!["bob@example.com", "maya@example.com"]
    );
}

#[test]
fn aliases_are_excluded_and_reply_to_unknown_parent_falls_back() {
    let me = vec![ME.to_string(), "lin.chen@work.example".to_string()];
    let emails = vec![
        // 回复的邮件不在本地：按参与人归组。
        mail(
            "r",
            1,
            "maya@example.com",
            &["lin.chen@work.example"],
            &[],
            "Re: Old",
            &["missing"],
        ),
        mail("n", 2, "maya@example.com", &[ME], &[], "New", &[]),
    ];
    let convs = group(&emails, &me);
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].messages, vec![0, 1]);
}

#[test]
fn reply_arriving_out_of_order_follows_parent_by_time() {
    // 输入顺序颠倒，但按时间处理后回复仍跟随父邮件，即使回复加了新的 To。
    let emails = vec![
        mail(
            "m2",
            1,
            "maya@example.com",
            &[ME, "alex@example.com"],
            &[],
            "Re: Plan",
            &["m1"],
        ),
        mail("m1", 0, "maya@example.com", &[ME], &[], "Plan", &[]),
    ];
    let convs = group(&emails, &[ME.into()]);
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].messages, vec![1, 0]);
}

#[test]
fn conversations_are_sorted_by_latest_activity() {
    let emails = vec![
        mail("a", 0, "old@example.com", &[ME], &[], "Old", &[]),
        mail("b", 5, "new@example.com", &[ME], &[], "New", &[]),
    ];
    let convs = group(&emails, &[ME.into()]);
    assert_eq!(addrs(&convs[0]), vec!["new@example.com"]);
}

#[test]
fn subject_prefixes_are_removed() {
    assert_eq!(base_subject("Re: Fwd: RE: Pilot"), "Pilot");
    assert_eq!(base_subject("回复：转发: 合同"), "合同");
    assert_eq!(base_subject("AW: WG: Angebot"), "Angebot");
    assert_eq!(base_subject("Report"), "Report");
}

#[test]
fn forward_detection_uses_subject_and_body_markers() {
    let fwd = mail("f", 0, "a@x.com", &[ME], &[], "Fwd: Hi", &[]);
    assert!(is_forward(&fwd));
    let mut body_fwd = mail("g", 0, "a@x.com", &[ME], &[], "Look at this", &[]);
    body_fwd.body = "FYI\n\n---------- Forwarded message ---------\nFrom: b@x.com".into();
    assert!(is_forward(&body_fwd));
    assert!(!is_forward(&mail(
        "h",
        0,
        "a@x.com",
        &[ME],
        &[],
        "Re: Hi",
        &[]
    )));
}

#[test]
fn samples_group_as_expected() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let mut emails = Vec::new();
    for name in [
        "maya-meeting.eml",
        "html-newsletter.eml",
        "forwarded-fyi.eml",
        "team-rsvp.eml",
    ] {
        emails.push(Email::parse(&std::fs::read(dir.join(name)).unwrap()).unwrap());
    }
    let convs = group(&emails, &[ME.into()]);
    // Maya 的两封（会议、HTML 计划）归入同一对话；Alex 的转发单独成对话；群发确认单独成对话。
    let maya = conv_of(&convs, 0);
    assert!(maya.messages.contains(&1));
    assert_eq!(maya.threads.len(), 2);
    assert_ne!(conv_of(&convs, 2).id, maya.id);
    assert_eq!(convs.len(), 3);
}
