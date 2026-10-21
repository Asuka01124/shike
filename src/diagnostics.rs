use crate::{engine::Event, store::Store};
use anyhow::Result;
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

/// Append only selected event fields: never serialize the session or reward codes.
pub fn record(dir: &Path, store: &Store, event: &Event) -> Result<()> {
    let now = chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
    let (kind, detail) = match event {
        Event::Prepared(_, _) => ("prepared", json!({})),
        Event::PreparationError(_, message) => ("preparation_error", json!({"message": message})),
        Event::Metadata(_, prepared) => (
            "metadata",
            json!({
                "fetched_at": prepared.fetched_at,
                "status": prepared.award.status,
                "message": prepared.award.message,
            }),
        ),
        Event::CoolingDown(_, until, failures) => (
            "preparation_cooling",
            json!({"until": until, "failures": failures}),
        ),
        Event::Waiting(_) => ("waiting", json!({})),
        Event::Started(_) => ("claim_started", json!({})),
        Event::Attempt(_, worker, attempt) => (
            "attempt_started",
            json!({"worker": worker + 1, "attempt": attempt}),
        ),
        Event::AttemptResult(_, result) => ("attempt_result", serde_json::to_value(result)?),
        Event::Message(..) => return Ok(()), // Same response is already logged with its worker and timing.
        Event::Archive(_, _, _) => ("archived", json!({})),
        Event::Failed(_, message) => ("failed", json!({"message": message})),
        Event::Paused(_) => ("paused", json!({})),
        Event::Finished(_) => ("finished", json!({})),
    };
    let task = store.tasks.iter().find(|task| task.id == event.task_id());
    let row = json!({
        "time": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, false),
        "task_id": event.task_id(),
        "target": task.map(|t| t.target),
        "event": kind,
        "detail": detail,
    });
    let logs = dir.join("logs");
    fs::create_dir_all(&logs)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join(format!("{}.jsonl", now.format("%Y-%m-%d"))))?;
    serde_json::to_writer(&mut file, &row)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::AttemptResult;
    #[test]
    fn history_survives_rescheduling_and_never_serializes_cookie_or_reward_code() {
        let dir = std::env::temp_dir().join(format!(
            "shike-journal-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mut store = Store::default();
        store.sign_in("SESSDATA=private-cookie".into(), "测试".into(), 1, 0);
        let id = store
            .add("https://www.bilibili.com/?task_id=test", 123)
            .unwrap();
        record(
            &dir,
            &store,
            &Event::AttemptResult(
                id,
                AttemptResult {
                    worker: 1,
                    attempt: 3,
                    started_at_ms: 1000,
                    elapsed_ms: 125,
                    code: Some(-702),
                    message: "请求频繁".into(),
                },
            ),
        )
        .unwrap();
        store.tasks[0].target = 456;
        store.tasks[0].attempts = [0; 2];
        record(
            &dir,
            &store,
            &Event::Archive(id, "领取成功".into(), Some("private-reward-code".into())),
        )
        .unwrap();
        let log = fs::read_dir(dir.join("logs"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = fs::read_to_string(&log).unwrap();
        assert!(!text.contains("private-cookie") && !text.contains("private-reward-code"));
        let rows: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["target"], 123);
        assert_eq!(rows[1]["target"], 456);
        assert_eq!(rows[0]["detail"]["code"], -702);
        assert_eq!(rows[0]["detail"]["elapsed_ms"], 125);
        fs::remove_file(log).unwrap();
        fs::remove_dir(dir.join("logs")).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
