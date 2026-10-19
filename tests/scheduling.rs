use serde_json::json;
use shike::{
    api::Api,
    engine::{self, Event},
    store::Store,
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

#[test]
fn multiple_links_start_at_target_and_archive_only_on_success() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let quit = Arc::new(AtomicBool::new(false));
    let stop = quit.clone();
    let posts = Arc::new(Mutex::new(Vec::new()));
    let log = posts.clone();
    let server = thread::spawn(move || {
        let mut handlers = Vec::new();
        while !stop.load(Ordering::Acquire) {
            if let Ok((mut s, _)) = listener.accept() {
                // Windows 接受的连接可能继承监听器的非阻塞模式。
                s.set_nonblocking(false).unwrap();
                let log = log.clone();
                handlers.push(thread::spawn(move||{
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();let mut buf=Vec::new();let mut chunk=[0u8;2048];let end;
            loop{let n=s.read(&mut chunk).unwrap();if n==0{return;}buf.extend_from_slice(&chunk[..n]);if let Some(p)=buf.windows(4).position(|b|b==b"\r\n\r\n"){end=p+4;break;}}
            let headers=String::from_utf8_lossy(&buf[..end]).to_lowercase();let len=headers.lines().find_map(|l|l.strip_prefix("content-length:").and_then(|s|s.trim().parse::<usize>().ok())).unwrap_or(0);
            while buf.len()<end+len{let n=s.read(&mut chunk).unwrap();if n==0{return;}buf.extend_from_slice(&chunk[..n]);}
            let body=if headers.starts_with("get /x/web-interface/nav "){
                    json!({"code":0,"data":{"isLogin":true,"uname":"测试用户","mid":1,"wbi_img":{"img_url":format!("https://x/{}.png","a".repeat(32)),"sub_url":format!("https://x/{}.png","b".repeat(32))}}})
            }else if headers.starts_with("get /x/activity_components/mission/info?"){
                json!({"code":0,"data":{"act_id":"act","act_name":"活动","task_name":"测试任务","status":0,"reward_info":{"award_name":"奖励"}}})
            }else{
                let form:std::collections::HashMap<_,_>=url::form_urlencoded::parse(&buf[end..]).into_owned().collect();
                assert!(headers.starts_with("post /x/activity_components/mission/receive?"));
                log.lock().unwrap().push((form["task_id"].clone(),chrono::Utc::now().timestamp_millis()));
                json!({"code":0,"data":{"award_type":"LiveOpen"}})
            }.to_string();
            let _=write!(s,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
        }));
            } else {
                thread::sleep(Duration::from_millis(5));
            }
        }
        for h in handlers {
            h.join().unwrap();
        }
    });
    let target = chrono::Utc::now().timestamp() + 2;
    let mut store = Store::default();
    store.sign_in(
        "test-session".into(),
        "测试用户".into(),
        1,
        chrono::Utc::now().timestamp(),
    );
    for id in ["one", "two"] {
        store
            .add(
                &format!(
                    "https://www.bilibili.com/blackboard/era/award-exchange.html?task_id={id}"
                ),
                target,
            )
            .unwrap();
    }
    let (tx, rx) = mpsc::channel();
    let mut controls = Vec::new();
    for task in store.tasks.clone() {
        controls.push(engine::start_with_api(
            task,
            Api::with_base("SESSDATA=test; bili_jct=csrf", &base).unwrap(),
            tx.clone(),
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut finished = 0;
    while finished < 2 && Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Event::Archive(id, message, key)) => store.archive(id, message, key),
            Ok(Event::Finished(_)) => finished += 1,
            Ok(Event::Failed(_, msg)) => panic!("不应失败：{msg}"),
            _ => {}
        }
    }
    for c in controls {
        c.store(true, Ordering::Release);
    }
    quit.store(true, Ordering::Release);
    server.join().unwrap();
    assert_eq!(finished, 2);
    assert!(
        store
            .tasks
            .iter()
            .all(|t| t.state == shike::store::TaskState::Archived)
    );
    let posts = posts.lock().unwrap();
    assert!((2..=4).contains(&posts.len()));
    for id in ["one", "two"] {
        let first = posts
            .iter()
            .filter(|p| p.0 == id)
            .map(|p| p.1)
            .min()
            .unwrap();
        assert!(first >= target * 1000);
        assert!(first < target * 1000 + 4000);
    }
}
