use serde_json::json;
use shike::{
    engine::Event,
    store::{Store, TaskState},
};

fn signed_in(id: u64, name: &str) -> Store {
    let mut store = Store::default();
    store.sign_in("test-session".into(), name.into(), id, 100);
    store
}
fn add(store: &mut Store, key: &str, time: i64) -> u64 {
    store
        .add(&format!("https://www.bilibili.com/?task_id={key}"), time)
        .unwrap()
}

#[test]
fn logout_preserves_ownership_manual_pause_and_records() {
    let mut s = signed_in(1, "账号甲");
    add(&mut s, "future", 500);
    add(&mut s, "paused", 600);
    let archived = add(&mut s, "received", 200);
    s.tasks[0].state = TaskState::Waiting;
    s.tasks[1].state = TaskState::Paused;
    s.archive(archived, "领取成功".into(), Some("demo-code".into()));
    s.sign_out();
    assert!(!s.is_logged_in());
    assert_eq!(s.tasks[0].state, TaskState::WaitingLogin);
    assert_eq!(s.tasks[1].state, TaskState::Paused);
    assert_eq!(s.tasks[2].state, TaskState::Archived);
    assert_eq!(s.tasks[2].cdkey.as_deref(), Some("demo-code"));
    assert!(
        s.tasks
            .iter()
            .all(|t| t.owner_id == 1 && t.owner_name == "账号甲" && !s.can_run(t))
    );
    assert!(s.add("https://www.bilibili.com/?task_id=new", 600).is_err());
}

#[test]
fn switching_accounts_never_reassigns_tasks_and_duplicates_are_per_account() {
    let mut s = signed_in(1, "账号甲");
    add(&mut s, "same-award", 500);
    s.sign_in("session-b".into(), "账号乙".into(), 2, 200);
    assert_eq!(s.tasks[0].state, TaskState::WaitingLogin);
    assert!(!s.can_run(&s.tasks[0]));
    add(&mut s, "same-award", 600);
    assert!(s.can_run(&s.tasks[1]));
    assert!(
        s.add("https://www.bilibili.com/?task_id=same-award", 700)
            .is_err()
    );
    s.sign_in("session-a".into(), "账号甲的新昵称".into(), 1, 300);
    assert!(s.can_run(&s.tasks[0]));
    assert_eq!(s.tasks[0].state, TaskState::Pending);
    assert_eq!(s.tasks[0].owner_name, "账号甲的新昵称");
    assert_eq!(s.tasks[1].state, TaskState::WaitingLogin);
    assert_eq!(s.tasks[1].owner_id, 2);
}

#[test]
fn same_account_resumes_future_only_and_keeps_manual_pause() {
    let mut s = signed_in(1, "账号甲");
    add(&mut s, "future", 800);
    add(&mut s, "missed", 250);
    add(&mut s, "paused", 900);
    s.tasks[2].state = TaskState::Paused;
    s.sign_out();
    s.sign_in("session-a".into(), "账号甲".into(), 1, 300);
    assert_eq!(s.tasks[0].state, TaskState::Pending);
    assert_eq!(s.tasks[1].state, TaskState::Failed);
    assert!(s.tasks[1].message.contains("错过"));
    assert_eq!(s.tasks[2].state, TaskState::Paused);
}

#[test]
fn late_worker_events_cannot_resume_logged_out_tasks_but_success_is_saved() {
    let mut s = signed_in(1, "账号甲");
    let id = add(&mut s, "in-flight", 500);
    s.tasks[0].state = TaskState::Running;
    s.sign_out();
    s.sign_in("session-b".into(), "账号乙".into(), 2, 300);
    assert!(!s.apply_event(&Event::Paused(id), true));
    assert!(!s.apply_event(&Event::Started(id), true));
    assert_eq!(s.tasks[0].state, TaskState::WaitingLogin);
    assert!(s.apply_event(
        &Event::Archive(id, "领取成功".into(), Some("demo-code".into())),
        true
    ));
    assert_eq!(s.tasks[0].state, TaskState::Archived);
    assert_eq!(s.tasks[0].owner_id, 1);
    assert_eq!(s.tasks[0].owner_name, "账号甲");
    assert!(!s.apply_event(&Event::Failed(id, "旧错误".into()), true));
}

#[test]
fn quick_relogin_keeps_automatic_resume_and_unknown_result_requires_review() {
    let mut s = signed_in(1, "账号甲");
    let id = add(&mut s, "future", 800);
    s.sign_out();
    s.sign_in("session-a".into(), "账号甲".into(), 1, 300);
    assert!(!s.apply_event(&Event::Paused(id), true));
    assert_eq!(s.tasks[0].state, TaskState::Pending);
    s.apply_event(&Event::Failed(id, "连接中断；请核对领奖记录".into()), true);
    s.sign_out();
    s.sign_in("session-a".into(), "账号甲".into(), 1, 350);
    assert_eq!(s.tasks[0].state, TaskState::Failed);
}

#[test]
fn legacy_tasks_migrate_to_original_account_and_logout_clears_saved_session() {
    let dir = std::env::temp_dir().join(format!("shike-account-migration-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = chrono::Utc::now().timestamp() + 3600;
    let legacy = json!({"cookie":"legacy-test-session","user_name":"原账号","user_id":42,"tasks":[{
        "id":1,"url":"https://www.bilibili.com/?task_id=old","task_id":"old",
        "title":"旧奖励","target":target,"state":"Waiting"
    }]});
    std::fs::write(dir.join("tasks.json"), legacy.to_string()).unwrap();
    let mut s = Store::load(&dir).unwrap();
    assert_eq!(s.tasks[0].owner_id, 42);
    assert_eq!(s.tasks[0].owner_name, "原账号");
    s.sign_out();
    s.save(&dir).unwrap();
    for name in ["tasks.json", "tasks.backup.json"] {
        let saved: Store = serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap();
        assert!(saved.cookie.is_empty());
        assert_eq!(saved.user_id, 0);
        assert_eq!(saved.tasks[0].owner_id, 42);
    }
    let mut loaded = Store::load(&dir).unwrap();
    loaded.sign_in("session-b".into(), "另一个账号".into(), 99, target - 100);
    assert!(!loaded.can_run(&loaded.tasks[0]));
    assert_eq!(loaded.tasks[0].owner_id, 42);
    for name in ["tasks.json", "tasks.backup.json"] {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn unknown_legacy_owner_is_bound_once_without_reassigning_old_records() {
    let mut s = Store::default();
    let raw = json!({"id":1,"url":"https://www.bilibili.com/?task_id=old","task_id":"old",
        "title":"未登录时添加的旧奖励","target":800,"state":"Pending"});
    s.tasks.push(serde_json::from_value(raw).unwrap());
    let unknown_record = json!({"id":2,"url":"https://www.bilibili.com/?task_id=record","task_id":"record",
        "title":"归属未知的旧记录","target":200,"state":"Archived"});
    s.tasks
        .push(serde_json::from_value(unknown_record).unwrap());
    s.sign_in("session-a".into(), "首次登录账号".into(), 1, 300);
    assert_eq!(s.tasks[0].owner_id, 1);
    s.sign_in("session-b".into(), "另一个账号".into(), 2, 400);
    assert_eq!(s.tasks[0].owner_id, 1);
    assert_eq!(s.tasks[0].state, TaskState::WaitingLogin);
    assert_eq!(s.tasks[1].owner_id, 0);
    s.sign_out();
    assert_eq!(s.tasks[1].owner_id, 0);
    let roundtrip: Store = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(roundtrip.schema_version, 2);
    assert_eq!(roundtrip.tasks[1].owner_id, 0);
}

#[test]
fn engine_rejects_a_session_for_another_account_before_reading_or_claiming_reward() {
    use shike::{api::Api, engine};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::Duration,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 2048];
        while !bytes.windows(4).any(|v| v == b"\r\n\r\n") {
            let n = socket.read(&mut chunk).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
        }
        assert!(String::from_utf8_lossy(&bytes).starts_with("GET /x/web-interface/nav "));
        let body = json!({"code":0,"data":{"isLogin":true,"uname":"账号乙","mid":2}}).to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    let mut store = signed_in(1, "账号甲");
    add(
        &mut store,
        "belongs-to-a",
        chrono::Utc::now().timestamp() + 60,
    );
    let (tx, rx) = mpsc::channel();
    let _cancel = engine::start_with_api(
        store.tasks[0].clone(),
        Api::with_base("SESSDATA=test; bili_jct=csrf", &base).unwrap(),
        tx,
    );
    let mut reason = None;
    loop {
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Failed(_, message) => reason = Some(message),
            Event::Finished(_) => break,
            Event::Prepared(..) | Event::Started(..) | Event::Archive(..) => {
                panic!("账号不符时不得准备或领取")
            }
            _ => {}
        }
    }
    assert!(reason.unwrap().contains("所属的账号"));
    server.join().unwrap();
}

#[test]
fn successful_inflight_response_after_logout_is_still_recorded_for_original_account() {
    use shike::{api::ClaimReply, engine};
    use std::{
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };
    let mut store = signed_in(1, "账号甲");
    let id = add(&mut store, "inflight", 800);
    store.tasks[0].state = TaskState::Running;
    let entered = Arc::new(Barrier::new(3));
    let release = Arc::new(Barrier::new(3));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let (entered_worker, release_worker, flag) = (entered.clone(), release.clone(), cancel.clone());
    let worker = thread::spawn(move || {
        engine::run_workers(
            id,
            Arc::new(move |_, _| {
                entered_worker.wait();
                release_worker.wait();
                Ok(ClaimReply::Success(None))
            }),
            flag,
            tx,
            Duration::from_millis(1),
            5,
        )
    });
    entered.wait();
    store.sign_out();
    cancel.store(true, Ordering::Release);
    store.sign_in("session-b".into(), "账号乙".into(), 2, 300);
    release.wait();
    worker.join().unwrap();
    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::Attempt(..)))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::Archive(..)))
            .count(),
        1
    );
    for event in events {
        store.apply_event(&event, true);
    }
    assert_eq!(store.tasks[0].state, TaskState::Archived);
    assert_eq!(store.tasks[0].owner_id, 1);
    assert_eq!(store.tasks[0].owner_name, "账号甲");
}
