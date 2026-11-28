//! `letterdesk-mock`：在本机启动模拟的微软登录 + Graph 与 Gmail IMAP/SMTP。
//!
//!   letterdesk-mock --mailbox samples --me lin@example.com --name "Lin Chen"

use mock_servers::{APP_PASSWORD, CLIENT_ID, Mailbox, MockServers};
use std::path::PathBuf;

fn main() {
    let mut mailbox_dir: Option<PathBuf> = None;
    let mut me = String::from("lin@example.com");
    let mut name = String::from("Lin Chen");
    let mut host = String::from("127.0.0.1");
    let (mut graph_port, mut imap_port, mut smtp_port) = (8901u16, 1143u16, 1025u16);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| fail(&format!("{arg} 需要一个值")))
        };
        match arg.as_str() {
            "--mailbox" => mailbox_dir = Some(value().into()),
            "--me" => me = value(),
            "--name" => name = value(),
            "--host" => host = value(),
            "--graph-port" => graph_port = value().parse().unwrap_or_else(|_| fail("端口无效")),
            "--imap-port" => imap_port = value().parse().unwrap_or_else(|_| fail("端口无效")),
            "--smtp-port" => smtp_port = value().parse().unwrap_or_else(|_| fail("端口无效")),
            "-h" | "--help" => {
                println!(
                    "用法：letterdesk-mock [--mailbox 目录] [--me 地址] [--name 名字] [--host 127.0.0.1] [--graph-port 8901] [--imap-port 1143] [--smtp-port 1025]"
                );
                return;
            }
            other => fail(&format!("未知参数：{other}")),
        }
    }

    let mailbox = Mailbox::new(&me, &name);
    if let Some(dir) = &mailbox_dir {
        match mailbox.load_dir(dir) {
            Ok(n) => println!("已从 {} 载入 {n} 封邮件", dir.display()),
            Err(e) => fail(&format!("无法读取 {}：{e}", dir.display())),
        }
    }
    let servers = MockServers::start(mailbox, &host, graph_port, imap_port, smtp_port)
        .unwrap_or_else(|e| fail(&format!("启动失败：{e}")));

    println!();
    println!("Outlook（微软登录 + Microsoft Graph）");
    println!("  登录与 Graph 基地址：{}", servers.graph.base_url);
    println!(
        "  授权：{}/common/oauth2/v2.0/authorize",
        servers.graph.base_url
    );
    println!(
        "  令牌：{}/common/oauth2/v2.0/token",
        servers.graph.base_url
    );
    println!("  应用（客户端）ID：{CLIENT_ID}");
    println!(
        "  投递新邮件：curl --data-binary @邮件.eml {}/mock/deliver",
        servers.graph.base_url
    );
    println!();
    println!("Gmail（明文，仅限本机开发）");
    println!("  IMAP：{host}:{}", servers.imap.port);
    println!("  SMTP：{host}:{}", servers.smtp.port);
    println!("  账号：{me}");
    println!("  应用专用密码：{APP_PASSWORD}");
    println!();
    println!("按 Ctrl+C 停止。");
    loop {
        std::thread::park();
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("错误：{msg}");
    std::process::exit(2);
}
