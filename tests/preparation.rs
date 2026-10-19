use serde_json::{Value, json};
use shike::{
    api::{Api, PreparationLimit},
    engine::{self, Event},
    store::{Store, TaskState},
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

struct Server {
    base: String,
    quit: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<(String, i64)>>>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(reply: impl Fn(&str, usize) -> (u16, String, Value) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let quit = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let (stop, entries) = (quit.clone(), log.clone());
        let reply = Arc::new(reply);
        let handle = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !stop.load(Ordering::Acquire) {
                if let Ok((mut s, _)) = listener.accept() {
                    s.set_nonblocking(false).unwrap();
                    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let (entries, reply) = (entries.clone(), reply.clone());
                    handlers.push(thread::spawn(move || {
                        let mut bytes = Vec::new(); let mut chunk = [0; 2048]; let end;
                        loop {
                            let n = s.read(&mut chunk).unwrap(); if n == 0 { return; }
                            bytes.extend_from_slice(&chunk[..n]);
                            if let Some(p) = bytes.windows(4).position(|b| b == b"\r\n\r\n") { end = p + 4; break; }
                        }
                        let header = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let size = header.lines().find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap_or(0);
                        while bytes.len() < end + size {
                            let n = s.read(&mut chunk).unwrap(); if n == 0 { return; } bytes.extend_from_slice(&chunk[..n]);
                        }
                        let path = header.lines().next().unwrap().to_string();
                        let index = {
                            let mut log = entries.lock().unwrap();
                            let count = log.iter().filter(|(p, _): &&(String, i64)| p.starts_with("get /x/activity_components/mission/info?")).count();
                            log.push((path.clone(), chrono::Utc::now().timestamp_millis())); count
                        };
                        let (status, extra, body) = reply(&path, index); let body = body.to_string();
                        write!(s, "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len()).unwrap();
                    }));
                } else {
                    thread::sleep(Duration::from_millis(5));
                }
            }
            for h in handlers {
                h.join().unwrap();
            }
        });
        Self {
            base,
            quit,
            log,
            handle: Some(handle),
        }
    }
    fn api(&self) -> Api {
        Api::with_base("SESSDATA=test; bili_jct=csrf", &self.base).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Release);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn ordinary(path: &str) -> Value {
    if path.starts_with("get /x/web-interface/nav ") {
        json!({"code":0,"data":{"isLogin":true,"uname":"测试账号","mid":1,"wbi_img":{"img_url":format!("https://x/{}.png", "a".repeat(32)),"sub_url":format!("https://x/{}.png", "b".repeat(32))}}})
    } else if path.starts_with("get ") {
        json!({"code":0,"data":{"act_id":"act","act_name":"活动","task_name":"任务","status":0,"reward_info":{"award_name":"奖励"}}})
    } else {
        json!({"code":0})
    }
}
fn store(target: i64) -> Store {
    let mut s = Store::default();
    s.sign_in("test".into(), "测试账号".into(), 1, 0);
    s.add("https://www.bilibili.com/?task_id=one", target)
        .unwrap();
    s
}

#[test]
fn read_429_carries_the_server_retry_after() {
    let server = Server::new(|_, _| (429, "Retry-After: 95\r\n".into(), json!({})));
    let error = server.api().account().unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<PreparationLimit>()
            .unwrap()
            .retry_after,
        95
    );
}

#[test]
fn restart_reuses_recent_award_metadata_and_refreshes_stale_or_other_link_cache() {
    let server = Server::new(|path, _| (200, String::new(), ordinary(path)));
    let dir = std::env::temp_dir().join(format!("shike-prepared-cache-{}", std::process::id()));
    let mut s = store(chrono::Utc::now().timestamp() + 3600);
    for round in 0..4 {
        let (tx, rx) = mpsc::channel();
        let cancel = engine::start_with_api(s.tasks[0].clone(), server.api(), tx);
        loop {
            let event = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let ready = matches!(event, Event::Waiting(_));
            s.apply_event(&event, false);
            if ready {
                break;
            }
        }
        cancel.store(true, Ordering::Release);
        loop {
            if matches!(
                rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                Event::Finished(_)
            ) {
                break;
            }
        }
        s.save(&dir).unwrap();
        s = Store::load(&dir).unwrap();
        assert!(s.tasks[0].prepared.is_some());
        let read_count = server
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with("get /x/activity_components/mission/info?"))
            .count();
        assert_eq!(read_count, [1, 1, 2, 3][round]);
        if round == 1 {
            s.tasks[0].prepared.as_mut().unwrap().fetched_at -= 301;
        }
        if round == 2 {
            s.tasks[0].task_id = "other-link".into();
        }
    }
    for name in ["tasks.json", "tasks.backup.json"] {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn json_limit_recovers_after_real_cooling_without_early_claim_and_shares_nav() {
    let server = Server::new(|path, index| {
        (
            200,
            String::new(),
            if path.starts_with("get /x/activity_components/mission/info?") && index == 0 {
                json!({"code":-702,"message":"请求频率过高"})
            } else {
                ordinary(path)
            },
        )
    });
    let target = chrono::Utc::now().timestamp() + 35;
    let mut s = store(target);
    s.add("https://www.bilibili.com/?task_id=two", target)
        .unwrap();
    let api = server.api();
    let (tx, rx) = mpsc::channel();
    let mut controls = Vec::new();
    for t in s.tasks.clone() {
        controls.push(engine::start_with_api(t, api.clone(), tx.clone()));
    }
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut finished = 0;
    let mut cooldowns = Vec::new();
    while finished < 2 && Instant::now() < deadline {
        if let Ok(event) = rx.recv_timeout(Duration::from_secs(1)) {
            match &event {
                Event::CoolingDown(_, until, _) => cooldowns.push(*until),
                Event::Finished(_) => finished += 1,
                Event::Failed(_, error) => panic!("不应失败: {error}"),
                _ => {}
            }
            s.apply_event(&event, false);
        }
    }
    for c in controls {
        c.store(true, Ordering::Release);
    }
    assert_eq!(finished, 2);
    assert!(!cooldowns.is_empty());
    assert!(
        s.tasks
            .iter()
            .all(|t| t.state == TaskState::Archived && t.target == target)
    );
    let log = server.log.lock().unwrap();
    let reads: Vec<_> = log
        .iter()
        .filter(|(p, _)| p.starts_with("get /x/activity_components/mission/info?"))
        .collect();
    assert_eq!(reads.len(), 3);
    assert!(reads[1].1 >= cooldowns[0] * 1000);
    assert!(reads.windows(2).all(|w| w[1].1 - w[0].1 >= 900));
    assert_eq!(
        log.iter()
            .filter(|(p, _)| p.starts_with("get /x/web-interface/nav "))
            .count(),
        1
    );
    let posts: Vec<_> = log.iter().filter(|(p, _)| p.starts_with("post ")).collect();
    assert!((2..=4).contains(&posts.len()));
    assert!(posts.iter().all(|p| p.1 >= target * 1000));
}

#[test]
fn saved_deadline_survives_restart_and_waiting_can_be_cancelled_without_requests() {
    let server = Server::new(|path, _| (200, String::new(), ordinary(path)));
    let mut s = store(chrono::Utc::now().timestamp() + 60);
    let until = chrono::Utc::now().timestamp() + 10;
    s.apply_event(&Event::CoolingDown(1, until, 2), false);
    let dir = std::env::temp_dir().join(format!("shike-cooling-{}", std::process::id()));
    s.save(&dir).unwrap();
    let mut restored = Store::load(&dir).unwrap();
    assert_eq!(restored.tasks[0].prep_retry_at, until);
    assert_eq!(restored.tasks[0].prep_failures, 2);
    let (tx, rx) = mpsc::channel();
    let cancel = engine::start_with_api(restored.tasks[0].clone(), server.api(), tx);
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Event::CoolingDown(1, at, 2) if at == until)
    );
    restored.sign_out();
    cancel.store(true, Ordering::Release);
    while let Ok(event) = rx.recv_timeout(Duration::from_secs(2)) {
        if matches!(event, Event::Finished(_)) {
            break;
        }
        restored.apply_event(&event, true);
    }
    assert!(server.log.lock().unwrap().is_empty());
    assert_eq!(restored.tasks[0].state, TaskState::WaitingLogin);
    restored.sign_in("new-session".into(), "另一个账号".into(), 2, 0);
    assert!(!restored.can_run(&restored.tasks[0]));
    assert_eq!(restored.tasks[0].prep_retry_at, until);
    std::fs::remove_file(dir.join("tasks.json")).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn login_verification_and_eligibility_errors_are_not_cooling_retries() {
    for code in [-101, 202100, 400] {
        let server = Server::new(move |_, _| {
            (
                200,
                String::new(),
                json!({"code":code,"message":"无法继续"}),
            )
        });
        let s = store(chrono::Utc::now().timestamp() + 60);
        let (tx, rx) = mpsc::channel();
        let _cancel = engine::start_with_api(s.tasks[0].clone(), server.api(), tx);
        let events: Vec<_> = rx.iter().collect();
        assert!(events.iter().any(|e| matches!(e, Event::Failed(..))));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::CoolingDown(..) | Event::Started(..)))
        );
        assert_eq!(server.log.lock().unwrap().len(), 1);
    }
}

#[test]
fn actual_claim_throttling_still_stops_instead_of_preparation_retry() {
    let server = Server::new(|path, _| {
        (
            200,
            String::new(),
            if path.starts_with("post ") {
                json!({"code":-702,"message":"频繁"})
            } else {
                ordinary(path)
            },
        )
    });
    let s = store(chrono::Utc::now().timestamp() + 2);
    let (tx, rx) = mpsc::channel();
    let _cancel = engine::start_with_api(s.tasks[0].clone(), server.api(), tx);
    let events: Vec<_> = rx.iter().collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Failed(_, msg) if msg.contains("限流")))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::CoolingDown(..) | Event::Archive(..)))
    );
    assert!(
        server
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with("post "))
            .count()
            <= 2
    );
}
