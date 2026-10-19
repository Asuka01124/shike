use crate::api::Api;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::net::TcpStream;
use std::{
    fs,
    path::Path,
    process::{Child, Command},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;
fn command(socket: &mut Socket, id: u64, method: &str, params: Value) -> Result<Value> {
    socket.send(Message::text(
        json!({"id":id,"method":method,"params":params}).to_string(),
    ))?;
    loop {
        let msg = socket.read()?;
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t)?;
            if v["id"].as_u64() == Some(id) {
                if !v["error"].is_null() {
                    bail!("浏览器读取失败");
                }
                return Ok(v["result"].clone());
            }
        }
    }
}
fn browser_path() -> Result<std::path::PathBuf> {
    for base in [
        std::env::var_os("ProgramFiles"),
        std::env::var_os("ProgramFiles(x86)"),
        std::env::var_os("LOCALAPPDATA"),
    ]
    .into_iter()
    .flatten()
    {
        for tail in [
            "Google/Chrome/Application/chrome.exe",
            "Microsoft/Edge/Application/msedge.exe",
        ] {
            let p = std::path::PathBuf::from(&base).join(tail);
            if p.exists() {
                return Ok(p);
            }
        }
    }
    bail!("未找到Chrome或Edge浏览器")
}
fn close(socket: &mut Socket, child: &mut Child, id: u64) {
    let _ = socket.send(Message::text(
        json!({"id":id,"method":"Browser.close"}).to_string(),
    ));
    let _ = child.try_wait();
}
pub fn login(
    dir: &Path,
    cancel: Arc<AtomicBool>,
    status: impl Fn(String),
) -> Result<(String, String, u64)> {
    let profile = dir.join("browser-profile");
    fs::create_dir_all(&profile)?;
    let port_file = profile.join("DevToolsActivePort");
    if port_file.exists() {
        fs::remove_file(&port_file)?;
    }
    status("请在浏览器中登录B站".into());
    let mut child = Command::new(browser_path()?)
        .arg(format!("--user-data-dir={}", profile.display()))
        .args([
            "--remote-debugging-port=0",
            "--remote-debugging-address=127.0.0.1",
            "--no-first-run",
            "--no-default-browser-check",
            "--new-window",
            "https://passport.bilibili.com/login?gourl=https%3A%2F%2Fwww.bilibili.com%2F",
        ])
        .spawn()
        .context("无法打开登录浏览器")?;
    let deadline = Instant::now() + Duration::from_secs(300);
    let port = loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            bail!("登录已取消");
        }
        if let Ok(s) = fs::read_to_string(&port_file) {
            if let Some(p) = s.lines().next().and_then(|s| s.parse::<u16>().ok()) {
                break p;
            }
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("浏览器启动超时");
        }
        thread::sleep(Duration::from_millis(150));
    };
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()?;
    let version: Value = http
        .get(format!("http://127.0.0.1:{port}/json/version"))
        .send()?
        .json()?;
    let ws = version["webSocketDebuggerUrl"]
        .as_str()
        .context("无法连接登录浏览器")?;
    let (mut socket, _) = tungstenite::connect(ws)?;
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    }
    let mut id = 0;
    while Instant::now() < deadline {
        if cancel.load(Ordering::Acquire) {
            close(&mut socket, &mut child, id + 1);
            bail!("登录已取消");
        }
        id += 1;
        let result = match command(&mut socket, id, "Storage.getCookies", json!({})) {
            Ok(v) => v,
            Err(_) => bail!("登录窗口已关闭，可重新登录"),
        };
        let mut cookies = std::collections::BTreeMap::new();
        if let Some(items) = result["cookies"].as_array() {
            for item in items {
                let domain = item["domain"].as_str().unwrap_or("");
                if domain == "bilibili.com" || domain.ends_with(".bilibili.com") {
                    if let (Some(k), Some(v)) = (item["name"].as_str(), item["value"].as_str()) {
                        cookies.insert(k.to_owned(), v.to_owned());
                    }
                }
            }
        }
        if cookies.contains_key("SESSDATA") && cookies.contains_key("bili_jct") {
            let cookie = cookies
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            match Api::new(&cookie)?.account() {
                Ok((name, mid)) => {
                    close(&mut socket, &mut child, id + 1);
                    return Ok((cookie, name, mid));
                }
                Err(_) => status("正在确认登录状态".into()),
            }
        }
        thread::sleep(Duration::from_millis(1500));
    }
    close(&mut socket, &mut child, id + 1);
    bail!("登录超时，请重新打开登录窗口")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_bridge_connects_without_touching_real_profile() {
        let dir = std::env::temp_dir().join(format!("shike-browser-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let port_file = dir.join("DevToolsActivePort");
        if port_file.exists() {
            fs::remove_file(&port_file).unwrap();
        }
        let mut child = Command::new(browser_path().unwrap())
            .arg(format!("--user-data-dir={}", dir.display()))
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-default-browser-check",
                "about:blank",
            ])
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let port = loop {
            if let Ok(s) = fs::read_to_string(&port_file) {
                if let Some(p) = s.lines().next().and_then(|s| s.parse::<u16>().ok()) {
                    break p;
                }
            }
            assert!(Instant::now() < deadline, "浏览器未启动");
            thread::sleep(Duration::from_millis(100));
        };
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let version: Value = http
            .get(format!("http://127.0.0.1:{port}/json/version"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        let (mut socket, _) =
            tungstenite::connect(version["webSocketDebuggerUrl"].as_str().unwrap()).unwrap();
        if let MaybeTlsStream::Plain(s) = socket.get_mut() {
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        let info = command(&mut socket, 1, "Browser.getVersion", json!({})).unwrap();
        assert!(info["product"].as_str().unwrap().contains("Chrome"));
        let cookies = command(&mut socket, 2, "Storage.getCookies", json!({})).unwrap();
        assert_eq!(cookies["cookies"].as_array().unwrap().len(), 0);
        close(&mut socket, &mut child, 3);
        let _ = child.wait();
        // 测试配置单独存放在临时目录；不读取或覆盖用户的浏览器配置。
    }
}
