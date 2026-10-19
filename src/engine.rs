use crate::{
    api::{Api, ClaimReply, PreparationLimit},
    store::{PreparedAward, Task},
};
use anyhow::Result;
use std::{
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::Sender,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum Event {
    Prepared(u64, String),
    Metadata(u64, PreparedAward),
    CoolingDown(u64, i64, u32),
    Waiting(u64),
    Started(u64),
    Attempt(u64, usize, u32),
    Message(u64, String),
    Archive(u64, String, Option<String>),
    Failed(u64, String),
    Paused(u64),
    Finished(u64),
}
impl Event {
    pub fn task_id(&self) -> u64 {
        match self {
            Self::Prepared(id, _)
            | Self::Metadata(id, _)
            | Self::CoolingDown(id, ..)
            | Self::Waiting(id)
            | Self::Started(id)
            | Self::Attempt(id, ..)
            | Self::Message(id, _)
            | Self::Archive(id, ..)
            | Self::Failed(id, _)
            | Self::Paused(id)
            | Self::Finished(id) => *id,
        }
    }
}
type ClaimFn = dyn Fn(usize, u32) -> Result<ClaimReply> + Send + Sync;
fn pause_until(deadline: Instant, cancel: &AtomicBool) {
    while Instant::now() < deadline && !cancel.load(Ordering::Acquire) {
        thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(25)),
        );
    }
}
pub fn run_workers(
    id: u64,
    claim: Arc<ClaimFn>,
    cancel: Arc<AtomicBool>,
    tx: Sender<Event>,
    period: Duration,
    max_attempts: u32,
) {
    let done = Arc::new(AtomicU8::new(0));
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for worker in 0..2 {
        let (claim, cancel, tx, done, barrier) = (
            claim.clone(),
            cancel.clone(),
            tx.clone(),
            done.clone(),
            barrier.clone(),
        );
        handles.push(thread::spawn(move || {
            barrier.wait();
            for attempt in 1..=max_attempts {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                let next = Instant::now() + period;
                let _ = tx.send(Event::Attempt(id, worker, attempt));
                match claim(worker, attempt) {
                    Ok(ClaimReply::Success(cdkey)) => {
                        if done.swap(2, Ordering::AcqRel) != 2 {
                            let _ = tx.send(Event::Archive(id, "领取成功".into(), cdkey));
                        }
                        cancel.store(true, Ordering::Release);
                        break;
                    }
                    Ok(ClaimReply::Already) => {
                        if done.swap(2, Ordering::AcqRel) != 2 {
                            let _ = tx.send(Event::Archive(id, "此前已领取".into(), None));
                        }
                        cancel.store(true, Ordering::Release);
                        break;
                    }
                    Ok(ClaimReply::Stop(code, message)) => {
                        if done
                            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                        {
                            let _ = tx.send(Event::Failed(id, format!("{message} · {code}")));
                        }
                        cancel.store(true, Ordering::Release);
                        break;
                    }
                    Ok(ClaimReply::Retry(code, message)) => {
                        let _ = tx.send(Event::Message(id, format!("{message} · {code}")));
                    }
                    Err(error) => {
                        if done
                            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                        {
                            let _ = tx.send(Event::Failed(id, format!("{error}；请核对领奖记录")));
                        }
                        cancel.store(true, Ordering::Release);
                        break;
                    }
                }
                pause_until(next, &cancel);
            }
        }));
    }
    barrier.wait();
    for handle in handles {
        let _ = handle.join();
    }
    if done.load(Ordering::Acquire) == 0 {
        let _ = tx.send(if cancel.load(Ordering::Acquire) {
            Event::Paused(id)
        } else {
            Event::Failed(id, "本轮结束，尚未领取".into())
        });
    }
}
pub fn start(task: Task, cookie: String, tx: Sender<Event>) -> Arc<AtomicBool> {
    match Api::new(&cookie) {
        Ok(api) => start_with_api(task, api, tx),
        Err(e) => {
            let _ = tx.send(Event::Failed(task.id, e.to_string()));
            let _ = tx.send(Event::Finished(task.id));
            Arc::new(AtomicBool::new(true))
        }
    }
}
pub fn start_with_api(task: Task, api: Api, tx: Sender<Event>) -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    thread::spawn(move || {
        let id = task.id;
        let result = (|| -> Result<()> {
            let mut failures = task.prep_failures;
            api.preparation
                .retry_at
                .fetch_max(task.prep_retry_at, Ordering::AcqRel);
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            let Some(prepared) = prepare(&task, &api, &stop, &tx, &mut failures, || {
                let (_, account_id) = api.account()?;
                if task.owner_id == 0 || task.owner_id != account_id {
                    anyhow::bail!("请登录这份奖励所属的账号后再领取");
                }
                if stop.load(Ordering::Acquire) {
                    anyhow::bail!("已取消准备");
                }
                let now = chrono::Utc::now().timestamp();
                if let Some(cached) = &task.prepared {
                    if cached.award.task_id == task.task_id
                        && (0..300).contains(&now.saturating_sub(cached.fetched_at))
                    {
                        return Ok(cached.clone());
                    }
                }
                Ok(PreparedAward {
                    award: api.award_info(&task.task_id)?,
                    fetched_at: chrono::Utc::now().timestamp(),
                })
            })?
            else {
                let _ = tx.send(Event::Paused(id));
                return Ok(());
            };
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            let award = prepared.award.clone();
            let _ = tx.send(Event::Metadata(id, prepared));
            let title = format!("{} · {}", award.task_name, award.reward_name);
            let _ = tx.send(Event::Prepared(id, title));
            if award.status == 6 {
                let _ = tx.send(Event::Archive(id, "此前已领取".into(), None));
                return Ok(());
            }
            let now = chrono::Utc::now().timestamp();
            if task.target < now - 60 {
                anyhow::bail!("已错过执行时间，请重新设置");
            }
            let _ = tx.send(Event::Waiting(id));
            failures = 0;
            let mut refreshed = false;
            while chrono::Utc::now().timestamp_millis() < task.target * 1000 {
                if stop.load(Ordering::Acquire) {
                    let _ = tx.send(Event::Paused(id));
                    return Ok(());
                }
                let left = task.target * 1000 - chrono::Utc::now().timestamp_millis();
                if left <= 15000 && !refreshed {
                    if prepare(&task, &api, &stop, &tx, &mut failures, || {
                        api.refresh_keys()
                    })?
                    .is_none()
                    {
                        let _ = tx.send(Event::Paused(id));
                        return Ok(());
                    }
                    let _ = tx.send(Event::Waiting(id));
                    refreshed = true;
                    continue;
                }
                thread::sleep(Duration::from_millis(if left > 1000 { 100 } else { 10 }));
            }
            if stop.load(Ordering::Acquire) {
                let _ = tx.send(Event::Paused(id));
                return Ok(());
            }
            if chrono::Utc::now().timestamp() > task.target.saturating_add(60) {
                anyhow::bail!("准备未能及时完成，请重新安排领取时间");
            }
            let _ = tx.send(Event::Started(id));
            let claim = Arc::new(move |_: usize, _: u32| api.claim(&award));
            run_workers(
                id,
                claim,
                stop.clone(),
                tx.clone(),
                Duration::from_secs(1),
                120,
            );
            Ok(())
        })();
        if let Err(error) = result {
            let _ = tx.send(if stop.load(Ordering::Acquire) {
                Event::Paused(id)
            } else {
                Event::Failed(id, error.to_string())
            });
        }
        let _ = tx.send(Event::Finished(id));
    });
    cancel
}

const MAX_PREPARATION_FAILURES: u32 = 6;
fn preparation_delay(failures: u32, server_delay: i64) -> i64 {
    (30_i64 << failures.saturating_sub(1).min(2)).max(server_delay)
}

/// Serializes preparation across all tasks sharing this login, never holding the gate while cooling.
fn prepare<T>(
    task: &Task,
    api: &Api,
    cancel: &AtomicBool,
    tx: &Sender<Event>,
    failures: &mut u32,
    action: impl Fn() -> Result<T>,
) -> Result<Option<T>> {
    let mut notified = None;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        let now = chrono::Utc::now().timestamp();
        if now > task.target.saturating_add(60) {
            anyhow::bail!("准备未能及时完成，请重新安排领取时间");
        }
        if *failures >= MAX_PREPARATION_FAILURES {
            anyhow::bail!("暂时无法读取奖励信息，请稍后重新安排");
        }
        let retry_at = api.preparation.retry_at.load(Ordering::Acquire);
        if retry_at > now {
            if notified != Some(retry_at) {
                let _ = tx.send(Event::CoolingDown(task.id, retry_at, *failures));
                notified = Some(retry_at);
            }
            thread::sleep(Duration::from_millis(25));
            continue;
        }
        let Ok(_gate) = api.preparation.gate.try_lock() else {
            thread::sleep(Duration::from_millis(25));
            continue;
        };
        // Another task may have entered cooling just before we acquired the gate.
        if api.preparation.retry_at.load(Ordering::Acquire) > chrono::Utc::now().timestamp() {
            continue;
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        match action() {
            Ok(value) => return Ok(Some(value)),
            Err(error) => {
                let Some(limit) = error.downcast_ref::<PreparationLimit>() else {
                    return Err(error);
                };
                *failures += 1;
                let until = chrono::Utc::now()
                    .timestamp()
                    .saturating_add(preparation_delay(*failures, limit.retry_after));
                api.preparation.retry_at.fetch_max(until, Ordering::AcqRel);
                let _ = tx.send(Event::CoolingDown(task.id, until, *failures));
                notified = Some(until);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicUsize, mpsc};
    #[test]
    fn preparation_backoff_respects_server_and_has_a_finite_budget() {
        assert_eq!(preparation_delay(1, 0), 30);
        assert_eq!(preparation_delay(2, 0), 60);
        assert_eq!(preparation_delay(3, 0), 120);
        assert_eq!(preparation_delay(5, 300), 300);
        let mut store = crate::store::Store::default();
        store.sign_in("test".into(), "测试".into(), 1, 0);
        store
            .add(
                "https://www.bilibili.com/?task_id=limited",
                chrono::Utc::now().timestamp() + 60,
            )
            .unwrap();
        let task = &store.tasks[0];
        let api = Api::with_base("test", "http://127.0.0.1:1").unwrap();
        let (tx, _) = mpsc::channel();
        let mut count = MAX_PREPARATION_FAILURES;
        assert!(
            prepare(
                task,
                &api,
                &AtomicBool::new(false),
                &tx,
                &mut count,
                || -> Result<()> { panic!("耗尽预算不得再请求") }
            )
            .unwrap_err()
            .to_string()
            .contains("稍后重新安排")
        );
        count = 0;
        let mut missed = task.clone();
        missed.target = chrono::Utc::now().timestamp() - 61;
        assert!(
            prepare(
                &missed,
                &api,
                &AtomicBool::new(false),
                &tx,
                &mut count,
                || -> Result<()> { panic!("过期不得再请求") }
            )
            .is_err()
        );
    }
    #[test]
    fn two_real_threads_and_message_success() {
        let gate = Arc::new(Barrier::new(2));
        let concurrent = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (g, c, p) = (gate.clone(), concurrent.clone(), peak.clone());
        let claim = Arc::new(move |_: usize, _: u32| {
            let n = c.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(n, Ordering::SeqCst);
            g.wait();
            c.fetch_sub(1, Ordering::SeqCst);
            Ok(ClaimReply::Success(None))
        });
        let (tx, rx) = mpsc::channel();
        run_workers(
            1,
            claim,
            Arc::new(AtomicBool::new(false)),
            tx,
            Duration::from_millis(10),
            2,
        );
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(
            rx.try_iter()
                .filter(|e| matches!(e, Event::Archive(..)))
                .count(),
            1
        );
    }
    #[test]
    fn cancelled_task_never_submits() {
        let claim = Arc::new(|_: usize, _: u32| -> Result<ClaimReply> { panic!("不应提交") });
        let (tx, rx) = mpsc::channel();
        run_workers(
            1,
            claim,
            Arc::new(AtomicBool::new(true)),
            tx,
            Duration::from_millis(1),
            2,
        );
        assert!(rx.try_iter().all(|e| !matches!(e, Event::Attempt(..))));
    }
    #[test]
    fn retry_is_bounded_and_one_second_policy_is_explicit() {
        let claim = Arc::new(|_: usize, _: u32| Ok(ClaimReply::Retry(400, "暂无库存".into())));
        let (tx, rx) = mpsc::channel();
        run_workers(
            1,
            claim,
            Arc::new(AtomicBool::new(false)),
            tx,
            Duration::from_millis(5),
            3,
        );
        assert_eq!(
            rx.try_iter()
                .filter(|e| matches!(e, Event::Attempt(..)))
                .count(),
            6
        );
    }
}
