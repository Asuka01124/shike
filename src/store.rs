use anyhow::{Result, bail};
use chrono::{FixedOffset, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum TaskState {
    #[default]
    Pending,
    Preparing,
    Waiting,
    Running,
    Paused,
    Failed,
    Archived,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub url: String,
    pub task_id: String,
    pub title: String,
    pub target: i64,
    pub state: TaskState,
    #[serde(default)]
    pub attempts: [u32; 2],
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub cdkey: Option<String>,
    #[serde(default)]
    pub archived_at: Option<i64>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub cookie: String,
    pub user_name: String,
    pub user_id: u64,
    pub tasks: Vec<Task>,
}
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("SHIKE_DATA_DIR") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("data")
}
impl Store {
    pub fn load(dir: &Path) -> Result<Self> {
        let file = dir.join("tasks.json");
        if !file.exists() {
            return Ok(Self::default());
        }
        let mut store: Self = serde_json::from_slice(&fs::read(file)?)?;
        for task in &mut store.tasks {
            if matches!(
                task.state,
                TaskState::Preparing | TaskState::Waiting | TaskState::Running
            ) {
                task.state = TaskState::Pending;
            }
        }
        Ok(store)
    }
    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let file = dir.join("tasks.json");
        let temporary = dir.join("tasks.tmp.json");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        // Windows不能原子覆盖已存在的文件；保留上一次可用副本。
        if file.exists() {
            fs::copy(&file, dir.join("tasks.backup.json"))?;
        }
        fs::rename(&temporary, &file).or_else(|_| fs::copy(&temporary, &file).map(|_| ()))?;
        Ok(())
    }
    pub fn add(&mut self, link: &str, target: i64) -> Result<u64> {
        let task_id = parse_task_id(link)?;
        if let Some(old) = self.tasks.iter().find(|t| t.task_id == task_id) {
            bail!(
                "{}",
                if old.state == TaskState::Archived {
                    "这个奖励已经领取"
                } else {
                    "这个任务已存在"
                }
            );
        }
        let id = self.tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        self.tasks.push(Task {
            id,
            url: format!(
                "https://www.bilibili.com/blackboard/era/award-exchange.html?task_id={task_id}"
            ),
            task_id,
            title: "新领取任务".into(),
            target,
            state: TaskState::Pending,
            attempts: [0; 2],
            message: "等待准备".into(),
            cdkey: None,
            archived_at: None,
        });
        Ok(id)
    }
    pub fn archive(&mut self, id: u64, message: String, cdkey: Option<String>) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.state = TaskState::Archived;
            task.message = message;
            task.cdkey = cdkey;
            task.archived_at = Some(Utc::now().timestamp());
        }
    }
}
pub fn parse_task_id(link: &str) -> Result<String> {
    let u = url::Url::parse(link.trim()).map_err(|_| anyhow::anyhow!("请粘贴奖励领取页链接"))?;
    if !u
        .host_str()
        .is_some_and(|h| h == "bilibili.com" || h.ends_with(".bilibili.com"))
    {
        bail!("请使用B站领取链接");
    }
    let id = u
        .query_pairs()
        .find(|(k, _)| k == "task_id" || k == "taskId")
        .map(|(_, v)| v.to_string())
        .filter(|s| !s.is_empty());
    id.ok_or_else(|| anyhow::anyhow!("这是活动首页，请复制具体奖励的领取页链接"))
}
pub fn china_time(ts: i64, format: &str) -> String {
    FixedOffset::east_opt(8 * 3600)
        .unwrap()
        .timestamp_opt(ts, 0)
        .single()
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}
pub fn next_midnight() -> String {
    let zone = FixedOffset::east_opt(8 * 3600).unwrap();
    let next = Utc::now()
        .with_timezone(&zone)
        .date_naive()
        .succ_opt()
        .unwrap();
    format!("{} 00:00:00", next.format("%Y-%m-%d"))
}
pub fn parse_time(s: &str) -> Result<i64> {
    let value = s.trim().replace('T', " ");
    let parsed = NaiveDateTime::parse_from_str(&value, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(&value, "%Y-%m-%d %H:%M"))
        .map_err(|_| anyhow::anyhow!("时间格式：2026-10-20 00:00:00"))?;
    Ok(FixedOffset::east_opt(8 * 3600)
        .unwrap()
        .from_local_datetime(&parsed)
        .single()
        .unwrap()
        .timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_and_archive() {
        let mut s = Store::default();
        let link = "https://www.bilibili.com/blackboard/era/award-exchange.html?task_id=demo";
        let id = s.add(link, 123).unwrap();
        assert!(s.add(link, 456).is_err());
        s.archive(id, "领取成功".into(), None);
        assert_eq!(s.tasks[0].state, TaskState::Archived);
        assert!(
            s.add(link, 456)
                .unwrap_err()
                .to_string()
                .contains("已经领取")
        );
    }
    #[test]
    fn parsing_and_timezone() {
        assert_eq!(parse_time("2026-10-20 00:00:00").unwrap(), 1792425600);
        assert_eq!(
            china_time(1792425600, "%Y-%m-%d %H:%M:%S"),
            "2026-10-20 00:00:00"
        );
        assert!(parse_task_id("https://evil.com/?task_id=test").is_err());
        assert!(parse_task_id("https://www.bilibili.com/blackboard/foo.html").is_err());
    }
    #[test]
    fn archives_survive_restart_and_active_tasks_resume() {
        let dir = std::env::temp_dir().join(format!(
            "shike-store-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mut s = Store::default();
        let id = s
            .add("https://www.bilibili.com/?task_id=archived", 123)
            .unwrap();
        s.archive(id, "领取成功".into(), Some("test-key".into()));
        s.add("https://www.bilibili.com/?task_id=pending", 456)
            .unwrap();
        s.tasks[1].state = TaskState::Waiting;
        s.save(&dir).unwrap();
        let loaded = Store::load(&dir).unwrap();
        assert_eq!(loaded.tasks[0].state, TaskState::Archived);
        assert_eq!(loaded.tasks[0].cdkey.as_deref(), Some("test-key"));
        assert_eq!(loaded.tasks[1].state, TaskState::Pending);
        std::fs::remove_file(dir.join("tasks.json")).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
