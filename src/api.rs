use anyhow::{Context, Result, bail};
use reqwest::{
    blocking::Client,
    cookie::{CookieStore, Jar},
    header::{REFERER, USER_AGENT},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, atomic::AtomicI64},
    time::{Duration, Instant},
};

const MIX: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];
#[derive(Clone)]
pub struct Api {
    client: Client,
    cookies: Arc<Jar>,
    cookie_url: url::Url,
    base: String,
    key: Arc<Mutex<Option<(String, Instant)>>>,
    account_cache: Arc<Mutex<Option<(String, u64, Instant)>>>,
    read_next: Arc<Mutex<Instant>>,
    pub(crate) preparation: Arc<Preparation>,
}
pub(crate) struct Preparation {
    pub gate: Mutex<()>,
    pub retry_at: AtomicI64,
}
/// Only explicit read-request throttling is recoverable by the preparation scheduler.
#[derive(Debug)]
pub struct PreparationLimit {
    pub retry_after: i64,
    pub message: String,
}
impl std::fmt::Display for PreparationLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for PreparationLimit {}

fn retry_after(value: Option<&str>, now: i64) -> i64 {
    value
        .and_then(|v| {
            v.trim().parse::<i64>().ok().or_else(|| {
                chrono::DateTime::parse_from_rfc2822(v)
                    .ok()
                    .map(|t| t.timestamp().saturating_sub(now))
            })
        })
        .unwrap_or(0)
        .max(0)
}

fn check_read(v: &Value, delay: i64) -> Result<()> {
    let code = v["code"].as_i64().unwrap_or(-99999);
    if code == 0 {
        return Ok(());
    }
    let message = format!(
        "{}（{code}）",
        v["message"].as_str().unwrap_or("读取信息失败")
    );
    if [-702, -509].contains(&code) {
        return Err(PreparationLimit {
            retry_after: delay,
            message,
        }
        .into());
    }
    bail!("{message}")
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Award {
    pub task_id: String,
    pub act_id: String,
    pub act_name: String,
    pub task_name: String,
    pub reward_name: String,
    pub status: i64,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimReply {
    Success(Option<String>),
    Already,
    Retry(i64, String),
}
pub fn normalize_cookie(cookie: &str) -> String {
    cookie
        .trim()
        .strip_prefix("Cookie:")
        .or_else(|| cookie.trim().strip_prefix("cookie:"))
        .unwrap_or(cookie.trim())
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}
pub fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    cookie
        .split(';')
        .filter_map(|s| s.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.to_string())
}
pub fn signed_query(params: &[(&str, &str)], key: &str, timestamp: i64) -> String {
    let mut map = BTreeMap::<String, String>::new();
    for (k, v) in params {
        map.insert(
            k.to_string(),
            v.chars().filter(|c| !"!'()*".contains(*c)).collect(),
        );
    }
    map.insert("wts".into(), timestamp.to_string());
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(map.iter())
        .finish();
    let rid = format!("{:x}", md5::compute(format!("{query}{key}")));
    format!("{query}&w_rid={rid}")
}
pub fn interpret_claim(v: &Value) -> ClaimReply {
    let code = v["code"].as_i64().unwrap_or(-99999);
    let msg = v["message"].as_str().unwrap_or("领取响应异常").to_string();
    if code == 0 {
        return ClaimReply::Success(
            v["data"]["extra_info"]["cdkey_content"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        );
    }
    if ["领取完", "领完", "抢完", "库存"]
        .iter()
        .any(|s| msg.contains(s))
    {
        return ClaimReply::Retry(code, msg);
    }
    if msg.contains("已领取") || msg.contains("已领奖") || msg.contains("已经领取") {
        return ClaimReply::Already;
    }
    ClaimReply::Retry(code, msg)
}
impl Api {
    pub fn restore_preparation_deadline(&self, until: i64) {
        self.preparation
            .retry_at
            .fetch_max(until, std::sync::atomic::Ordering::AcqRel);
    }
    pub fn new(cookie: &str) -> Result<Self> {
        Self::with_base(cookie, "https://api.bilibili.com")
    }
    pub fn with_base(cookie: &str, base: &str) -> Result<Self> {
        let cookie = normalize_cookie(cookie);
        let cookie_url = url::Url::parse(&format!("{}/", base.trim_end_matches('/')))?;
        let cookies = Arc::new(Jar::default());
        for part in cookie.split(';').map(str::trim).filter(|p| p.contains('=')) {
            cookies.add_cookie_str(&format!("{part}; Path=/"), &cookie_url);
        }
        Ok(Self {
            client: Client::builder()
                .cookie_provider(cookies.clone())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(8))
                .pool_idle_timeout(Duration::from_secs(90))
                .build()?,
            cookies,
            cookie_url,
            base: base.into(),
            key: Arc::new(Mutex::new(None)),
            account_cache: Arc::new(Mutex::new(None)),
            read_next: Arc::new(Mutex::new(Instant::now())),
            preparation: Arc::new(Preparation {
                gate: Mutex::new(()),
                retry_at: AtomicI64::new(0),
            }),
        })
    }
    fn get(&self, path: &str) -> Result<Value> {
        let mut next = self.read_next.lock().unwrap();
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        *next = Instant::now() + Duration::from_secs(1);
        let r=self.client.get(format!("{}{path}",self.base)).header(REFERER,"https://www.bilibili.com/").header(USER_AGENT,"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/144.0.0.0 Safari/537.36").send()?;
        let delay = retry_after(
            r.headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok()),
            chrono::Utc::now().timestamp(),
        );
        if r.status().as_u16() == 429 {
            return Err(PreparationLimit {
                retry_after: delay,
                message: "读取信息暂时受限 HTTP 429".into(),
            }
            .into());
        }
        if !r.status().is_success() {
            bail!("接口 HTTP {}", r.status().as_u16());
        }
        let value = r.json()?;
        check_read(&value, delay)?;
        Ok(value)
    }
    pub fn account(&self) -> Result<(String, u64)> {
        if let Some((name, id, at)) = &*self.account_cache.lock().unwrap() {
            if at.elapsed() < Duration::from_secs(300) {
                return Ok((name.clone(), *id));
            }
        }
        let v = self.get("/x/web-interface/nav")?;
        self.cache_keys(&v);
        if !v["data"]["isLogin"].as_bool().unwrap_or(false) {
            bail!("请先登录B站账号");
        }
        if self.csrf().is_empty() {
            bail!("登录信息不完整，请重新登录");
        }
        let account: (String, u64) = (
            v["data"]["uname"].as_str().unwrap_or("B站用户").into(),
            v["data"]["mid"].as_u64().unwrap_or(0),
        );
        *self.account_cache.lock().unwrap() = Some((account.0.clone(), account.1, Instant::now()));
        Ok(account)
    }
    fn cache_keys(&self, v: &Value) {
        let img = v["data"]["wbi_img"]["img_url"].as_str().unwrap_or("");
        let sub = v["data"]["wbi_img"]["sub_url"].as_str().unwrap_or("");
        let extract = |s: &str| {
            s.rsplit('/')
                .next()
                .unwrap_or("")
                .split('.')
                .next()
                .unwrap_or("")
                .to_string()
        };
        let joined = extract(img) + &extract(sub);
        if joined.len() >= 64 {
            let key = MIX
                .iter()
                .take(32)
                .map(|&i| joined.as_bytes()[i] as char)
                .collect::<String>();
            *self.key.lock().unwrap() = Some((key, Instant::now()));
        }
    }
    pub fn refresh_keys(&self) -> Result<()> {
        // Nearby schedules share fresh keys; do not issue one NAV per task before midnight.
        if self
            .key
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(1800))
        {
            return Ok(());
        }
        let v = self.get("/x/web-interface/nav")?;
        self.cache_keys(&v);
        self.wbi_key().map(|_| ())
    }
    fn wbi_key(&self) -> Result<String> {
        if let Some((key, created)) = &*self.key.lock().unwrap() {
            if created.elapsed() < Duration::from_secs(3600) {
                return Ok(key.clone());
            }
        }
        let v = self.get("/x/web-interface/nav")?;
        self.cache_keys(&v);
        self.key
            .lock()
            .unwrap()
            .as_ref()
            .map(|(k, _)| k.clone())
            .context("无法获取请求签名")
    }
    pub fn award_info(&self, task: &str) -> Result<Award> {
        let key = self.wbi_key()?;
        let query = signed_query(
            &[("task_id", task), ("web_location", "888.126558")],
            &key,
            chrono::Utc::now().timestamp(),
        );
        let v = self.get(&format!("/x/activity_components/mission/info?{query}"))?;
        if v["code"].as_i64() != Some(0) {
            bail!(
                "{}（{}）",
                v["message"].as_str().unwrap_or("读取奖励失败"),
                v["code"]
            );
        }
        let d = &v["data"];
        let a = Award {
            task_id: task.into(),
            act_id: d["act_id"].as_str().unwrap_or("").into(),
            act_name: d["act_name"].as_str().unwrap_or("").into(),
            task_name: d["task_name"].as_str().unwrap_or("").into(),
            reward_name: d["reward_info"]["award_name"].as_str().unwrap_or("").into(),
            status: d["status"].as_i64().unwrap_or(-1),
            message: d["message"].as_str().unwrap_or("").into(),
        };
        if a.act_id.is_empty() || a.reward_name.is_empty() {
            bail!("该链接没有可用的奖励信息");
        }
        Ok(a)
    }
    pub fn claim(&self, a: &Award) -> Result<ClaimReply> {
        let key = self.wbi_key()?;
        let csrf = self.csrf();
        let query = signed_query(&[], &key, chrono::Utc::now().timestamp());
        let r=self.client.post(format!("{}/x/activity_components/mission/receive?{query}",self.base))
            .header(REFERER,"https://www.bilibili.com/")
            .header(USER_AGENT,"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/144.0.0.0 Safari/537.36")
            .form(&[("task_id",a.task_id.as_str()),("activity_id",a.act_id.as_str()),("activity_name",a.act_name.as_str()),("task_name",a.task_name.as_str()),
                ("reward_name",a.reward_name.as_str()),("gaia_vtoken",""),("receive_from","missionPage"),("csrf",csrf.as_str())]).send()?;
        let status = r.status().as_u16();
        if !r.status().is_success() {
            if let Ok(body) = r.json::<Value>() {
                if let Some(message) = body["message"].as_str() {
                    let message: String = message.chars().take(512).collect();
                    if let Some(code) = body["code"].as_i64() {
                        bail!("领取响应 HTTP {status}；{message} · {code}");
                    }
                    bail!("领取响应 HTTP {status}；{message}");
                }
            }
            bail!("领取响应 HTTP {status}");
        }
        Ok(interpret_claim(&r.json()?))
    }
    fn csrf(&self) -> String {
        self.cookies
            .cookies(&self.cookie_url)
            .and_then(|header| {
                header
                    .to_str()
                    .ok()
                    .and_then(|s| cookie_value(s, "bili_jct"))
            })
            .unwrap_or_default()
    }
    pub fn history(&self, a: &Award) -> Result<Value> {
        let q = signed_query(
            &[("activity_id", a.act_id.as_str())],
            &self.wbi_key()?,
            chrono::Utc::now().timestamp(),
        );
        self.get(&format!("/x/activity_components/mission/mylist?{q}"))
    }
    pub fn probe_anonymous(&self, task: &str) -> Result<Value> {
        let a = self.award_info(task)?;
        Ok(json!({"task":a.task_id,"title":a.task_name,"message":a.message,"status":a.status}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn successful_message_delivery_needs_no_cdkey() {
        assert_eq!(
            interpret_claim(&json!({"code":0,"data":{"award_type":"LiveOpen"}})),
            ClaimReply::Success(None)
        );
    }
    #[test]
    fn all_failure_codes_retry_and_already_received_finishes() {
        for code in [
            -101, 61000, 202100, 202101, 202102, -509, -702, -412, 400, 99999,
        ] {
            assert_eq!(
                interpret_claim(&json!({"code":code,"message":"原始失败原因"})),
                ClaimReply::Retry(code, "原始失败原因".into())
            );
        }
        for message in ["活动已结束", "活动不存在"] {
            assert_eq!(
                interpret_claim(&json!({"code":400,"message":message})),
                ClaimReply::Retry(400, message.into())
            );
        }
        assert_eq!(
            interpret_claim(&json!({"code":202005,"message":"已领取"})),
            ClaimReply::Already
        );
        assert!(matches!(
            interpret_claim(&json!({"code":400,"message":"库存不足"})),
            ClaimReply::Retry(400, _)
        ));
        assert!(matches!(
            interpret_claim(&json!({"code":400,"message":"今日奖品已领取完毕"})),
            ClaimReply::Retry(400, _)
        ));
    }
    #[test]
    fn only_explicit_read_throttles_are_recoverable_and_retry_after_is_parsed() {
        for code in [-702, -509] {
            let error = check_read(&json!({"code":code,"message":"请求频繁"}), 90).unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<PreparationLimit>()
                    .unwrap()
                    .retry_after,
                90
            );
        }
        for code in [-101, 202100, -412, 400] {
            assert!(
                check_read(&json!({"code":code}), 0)
                    .unwrap_err()
                    .downcast_ref::<PreparationLimit>()
                    .is_none()
            );
        }
        assert_eq!(retry_after(Some("90"), 0), 90);
        assert_eq!(
            retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT"), 1445412420),
            60
        );
        assert_eq!(retry_after(Some("-2"), 0), 0);
        assert_eq!(retry_after(Some("invalid"), 0), 0);
    }
    #[test]
    fn csrf_and_signatures() {
        assert_eq!(
            cookie_value("SESSDATA=abc==; bili_jct=token; foo=x", "bili_jct"),
            Some("token".into())
        );
        assert_eq!(normalize_cookie("Cookie: A=b; C=d;"), "A=b; C=d");
        let q = signed_query(&[("z", "x!"), ("a", "test")], "key", 100);
        let base = "a=test&wts=100&z=x";
        assert_eq!(
            q,
            format!("{base}&w_rid={:x}", md5::compute(format!("{base}key")))
        );
    }
    #[test]
    fn server_cookie_updates_are_reused_and_csrf_matches_after_a_failed_claim() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for index in 0..4 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 2048];
                let end = loop {
                    let count = socket.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length = headers
                    .lines()
                    .find_map(|s| {
                        s.strip_prefix("content-length:")
                            .and_then(|s| s.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < end + length {
                    let count = socket.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let cookie = headers
                    .lines()
                    .find_map(|s| s.strip_prefix("cookie: "))
                    .unwrap();
                assert_eq!(cookie_value(cookie, "sessdata").as_deref(), Some("test"));
                if index > 0 {
                    let csrf = if index == 3 { "after-failure" } else { "fresh" };
                    assert_eq!(cookie_value(cookie, "bili_jct").as_deref(), Some(csrf));
                    assert_eq!(
                        cookie_value(cookie, "server-session").as_deref(),
                        Some("updated")
                    );
                    if index >= 2 {
                        assert!(
                            headers.starts_with("post /x/activity_components/mission/receive?")
                        );
                        assert!(!headers.lines().any(|h| h.starts_with("origin:")));
                        let form: BTreeMap<_, _> = url::form_urlencoded::parse(&bytes[end..])
                            .into_owned()
                            .collect();
                        assert_eq!(form["csrf"], csrf);
                    }
                }
                let (status, extra, value) = match index {
                    0 => (
                        200,
                        "Set-Cookie: bili_jct=fresh; Path=/\r\nSet-Cookie: server-session=updated; Path=/\r\n",
                        json!({"code":0,"data":{"isLogin":true,"uname":"测试","mid":1,"wbi_img":{"img_url":format!("https://x/{}.png", "a".repeat(32)),"sub_url":format!("https://x/{}.png", "b".repeat(32))}}}),
                    ),
                    1 => (
                        200,
                        "",
                        json!({"code":0,"data":{"act_id":"act","act_name":"活动","task_name":"任务","reward_info":{"award_name":"奖励"}}}),
                    ),
                    2 => (
                        429,
                        "Set-Cookie: bili_jct=after-failure; Path=/\r\n",
                        json!({"code":-702}),
                    ),
                    _ => (200, "", json!({"code":0})),
                };
                let body = value.to_string();
                write!(socket, "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len()).unwrap();
            }
        });
        let api = Api::with_base(
            "SESSDATA=test; bili_jct=initial",
            &format!("http://{address}"),
        )
        .unwrap();
        api.account().unwrap();
        let award = api.award_info("task").unwrap();
        assert!(
            api.claim(&award)
                .unwrap_err()
                .to_string()
                .contains("HTTP 429")
        );
        assert_eq!(api.claim(&award).unwrap(), ClaimReply::Success(None));
        server.join().unwrap();
    }
    #[test]
    fn http_flow_sends_cookie_csrf_and_signed_post() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut claims = 0;
            for _ in 0..3 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buf = [0u8; 2048];
                let header_end;
                loop {
                    let n = socket.read(&mut buf).unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&buf[..n]);
                    if let Some(i) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                        header_end = i + 4;
                        break;
                    }
                }
                let headers = String::from_utf8_lossy(&data[..header_end]).to_lowercase();
                let cookie = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("cookie: "))
                    .unwrap();
                assert_eq!(cookie_value(cookie, "sessdata").as_deref(), Some("test"));
                assert_eq!(
                    cookie_value(cookie, "bili_jct").as_deref(),
                    Some("csrf-token")
                );
                let len = headers
                    .lines()
                    .find_map(|s| {
                        s.strip_prefix("content-length:")
                            .and_then(|s| s.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while data.len() < header_end + len {
                    let n = socket.read(&mut buf).unwrap();
                    data.extend_from_slice(&buf[..n]);
                }
                let response=if headers.starts_with("get /x/web-interface/nav ") {
                    json!({"code":0,"data":{"isLogin":true,"uname":"测试账号","mid":1,"wbi_img":{"img_url":format!("https://i.example/{}.png","a".repeat(32)),"sub_url":format!("https://i.example/{}.png","b".repeat(32))}}})
                }else if headers.starts_with("get /x/activity_components/mission/info?") {
                    assert!(headers.contains("w_rid="));json!({"code":0,"data":{"act_id":"act","act_name":"活动","task_name":"任务","status":0,"reward_info":{"award_name":"奖品"}}})
                }else{
                    assert!(headers.starts_with("post /x/activity_components/mission/receive?"));assert!(headers.contains("w_rid="));
                    let form:url::form_urlencoded::Parse<'_>=url::form_urlencoded::parse(&data[header_end..]);let form:std::collections::HashMap<_,_>=form.into_owned().collect();
                    assert_eq!(form.get("csrf").unwrap(),"csrf-token");assert_eq!(form.get("task_id").unwrap(),"task");assert_eq!(form.get("receive_from").unwrap(),"missionPage");claims+=1;
                    json!({"code":0,"data":{"award_type":"LiveOpen"}})
                }.to_string();
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
            }
            assert_eq!(claims, 1);
        });
        let api = Api::with_base(
            "SESSDATA=test; bili_jct=csrf-token",
            &format!("http://{address}"),
        )
        .unwrap();
        assert_eq!(api.account().unwrap().0, "测试账号");
        let award = api.award_info("task").unwrap();
        assert_eq!(api.claim(&award).unwrap(), ClaimReply::Success(None));
        server.join().unwrap();
    }
}
