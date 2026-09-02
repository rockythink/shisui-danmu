use crate::domain::{DanmuEvent, DanmuSession, DanmuSessionEndReason};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalRecord {
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub kind: JournalKind,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum JournalKind {
    SessionStarted,
    EventReceived,
    SessionSnapshot,
    SessionEnded,
    SessionInterrupted,
    UnhandledCommand,
}

#[derive(Debug, Clone)]
pub struct SessionJournal {
    root: PathBuf,
}

impl SessionJournal {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn start(&self, session: &DanmuSession) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::SessionStarted,
            serde_json::to_value(session)?,
        )
    }

    pub fn event(&self, session: &DanmuSession, event: &DanmuEvent) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::EventReceived,
            serde_json::to_value(event)?,
        )?;
        self.snapshot(session)
    }

    pub fn unhandled_command(&self, session: &DanmuSession, command: &str) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::UnhandledCommand,
            serde_json::json!({ "command": command }),
        )
    }

    pub fn snapshot(&self, session: &DanmuSession) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::SessionSnapshot,
            serde_json::to_value(session)?,
        )
    }

    pub fn end(&self, session: &DanmuSession) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::SessionEnded,
            serde_json::to_value(session)?,
        )?;
        self.write_exports(session)
    }

    pub fn interrupt(&self, session: &DanmuSession) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::SessionInterrupted,
            serde_json::to_value(session)?,
        )
    }

    pub fn recover_latest_interrupted(&self) -> Result<Option<DanmuSession>> {
        let mut candidates = Vec::new();
        for id in self.session_ids()? {
            if let Some(session) = self.latest_session(&id)? {
                candidates.push(session);
            }
        }
        candidates.sort_by_key(|session| session.started_at);
        Ok(candidates
            .into_iter()
            .rev()
            .find(|session| session.ended_at.is_none()))
    }

    pub fn search(&self, query: &str) -> Result<Vec<DanmuSession>> {
        let query = query.trim().to_lowercase();
        let mut output = Vec::new();
        for id in self.session_ids()? {
            if let Some(session) = self.latest_session(&id)? {
                let matches = query.is_empty()
                    || session.room_id.to_lowercase().contains(&query)
                    || session.recent_events.iter().any(|event| {
                        event.content.to_lowercase().contains(&query)
                            || event
                                .username
                                .as_deref()
                                .unwrap_or_default()
                                .to_lowercase()
                                .contains(&query)
                    });
                if matches {
                    output.push(session);
                }
            }
        }
        output.sort_by_key(|session| std::cmp::Reverse(session.started_at));
        Ok(output)
    }

    pub fn write_exports(&self, session: &DanmuSession) -> Result<()> {
        let directory = self.session_directory(&session.id);
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("snapshot.json"),
            serde_json::to_vec_pretty(session)?,
        )?;
        let markdown = format!(
            "# 直播会话 {}

- 房间：{}
- 开始：{}
- 结束：{}
- 事件：{}
",
            session.id,
            session.room_id,
            session.started_at.to_rfc3339(),
            session
                .ended_at
                .map(|value| value.to_rfc3339())
                .unwrap_or_else(|| "未结束".into()),
            session.metrics.total_event_count,
        );
        std::fs::write(directory.join("summary.md"), markdown)?;
        Ok(())
    }

    fn append<T: Serialize>(&self, session_id: &str, kind: JournalKind, payload: T) -> Result<()> {
        let directory = self.session_directory(session_id);
        std::fs::create_dir_all(&directory).context("创建会话归档目录失败")?;
        let path = directory.join("journal.jsonl");
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        file.lock_exclusive()?;
        let sequence = count_lines(&file)? + 1;
        let record = JournalRecord {
            sequence,
            timestamp: Utc::now(),
            kind,
            payload: serde_json::to_value(payload)?,
        };
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        FileExt::unlock(&file)?;
        Ok(())
    }

    fn latest_session(&self, id: &str) -> Result<Option<DanmuSession>> {
        let path = self.session_directory(id).join("journal.jsonl");
        let records = read_records(&path)?;
        for record in records.into_iter().rev() {
            if matches!(
                record.kind,
                JournalKind::SessionStarted
                    | JournalKind::SessionSnapshot
                    | JournalKind::SessionEnded
                    | JournalKind::SessionInterrupted
            ) && let Ok(mut session) = serde_json::from_value::<DanmuSession>(record.payload)
            {
                session.restore_runtime_state();
                return Ok(Some(session));
            }
        }
        replay_legacy_session(&path, id)
    }

    fn session_ids(&self) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        Ok(entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && entry.path().join("journal.jsonl").is_file()
            })
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect())
    }

    fn session_directory(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
}

fn count_lines(file: &File) -> Result<u64> {
    let cloned = file.try_clone()?;
    Ok(BufReader::new(cloned).lines().count() as u64)
}

fn read_records(path: &Path) -> Result<Vec<JournalRecord>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect::<Vec<_>>()
        .pipe(Ok)
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyJournalRecord {
    recorded_at: DateTime<Utc>,
    kind: String,
    payload: Value,
}

fn replay_legacy_session(path: &Path, session_id: &str) -> Result<Option<DanmuSession>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut session: Option<DanmuSession> = None;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<LegacyJournalRecord>(&line) else {
            continue;
        };
        match record.kind.as_str() {
            "sessionStarted" => {
                let Some(room_id) = record.payload.get("roomID").and_then(Value::as_str) else {
                    continue;
                };
                let mut restored = DanmuSession::with_options(room_id, 240, record.recorded_at);
                restored.id = session_id.to_owned();
                session = Some(restored);
            }
            "eventReceived" => {
                if let (Some(restored), Some(event)) = (
                    session.as_mut(),
                    record
                        .payload
                        .get("event")
                        .and_then(|value| serde_json::from_value::<DanmuEvent>(value.clone()).ok()),
                ) {
                    restored.ingest(event);
                }
            }
            "featuredChanged" => {
                if let Some(restored) = session.as_mut() {
                    restored.feature(
                        record
                            .payload
                            .get("featuredEventID")
                            .and_then(Value::as_str),
                    );
                }
            }
            "sessionInterrupted" => {
                if let Some(restored) = session.as_mut() {
                    restored.interrupt();
                }
            }
            "sessionResumed" => {
                if let Some(restored) = session.as_mut() {
                    restored.resume();
                }
            }
            "sessionEnded" => {
                if let Some(restored) = session.as_mut() {
                    let reason = record
                        .payload
                        .get("endReason")
                        .cloned()
                        .and_then(|value| {
                            serde_json::from_value::<DanmuSessionEndReason>(value).ok()
                        })
                        .unwrap_or(DanmuSessionEndReason::Completed);
                    restored.end(record.recorded_at, reason);
                }
            }
            _ => {}
        }
    }
    if let Some(restored) = session.as_mut() {
        restored.restore_runtime_state();
    }
    Ok(session)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DanmuEvent, DanmuEventKind};

    #[test]
    fn persists_and_recovers_active_session() {
        let temp = tempfile::tempdir().unwrap();
        let journal = SessionJournal::new(temp.path().into());
        let mut session = DanmuSession::new("42");
        journal.start(&session).unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "怎么做？");
        session.ingest(event.clone());
        journal.event(&session, &event).unwrap();
        assert_eq!(
            journal
                .recover_latest_interrupted()
                .unwrap()
                .unwrap()
                .room_id,
            "42"
        );
    }

    #[test]
    fn persists_only_the_unhandled_command_name() {
        let temp = tempfile::tempdir().unwrap();
        let journal = SessionJournal::new(temp.path().into());
        let session = DanmuSession::new("42");
        journal.start(&session).unwrap();
        journal.unhandled_command(&session, "ROOM_CHANGE").unwrap();

        let records =
            read_records(&journal.session_directory(&session.id).join("journal.jsonl")).unwrap();
        let diagnostic = records.last().unwrap();
        assert_eq!(diagnostic.kind, JournalKind::UnhandledCommand);
        assert_eq!(
            diagnostic.payload,
            serde_json::json!({ "command": "ROOM_CHANGE" })
        );
    }

    #[test]
    fn replays_swift_v1_journal_and_rebuilds_deduplication() {
        let temp = tempfile::tempdir().unwrap();
        let session_id = "legacy-session";
        let directory = temp.path().join(session_id);
        std::fs::create_dir_all(&directory).unwrap();
        let now = Utc::now();
        let event = serde_json::json!({
            "id": "legacy-event", "kind": "danmu", "timestamp": now,
            "username": "观众", "authorID": "42", "content": "这是问题吗？", "platformEventID": null
        });
        let records = [
            serde_json::json!({"schemaVersion":1,"recordID":"a","sessionID":session_id,"sequence":1,"recordedAt":now,"kind":"sessionStarted","payload":{"roomID":"123"}}),
            serde_json::json!({"schemaVersion":1,"recordID":"b","sessionID":session_id,"sequence":2,"recordedAt":now,"kind":"eventReceived","payload":{"event":event}}),
        ];
        let text = records
            .into_iter()
            .map(|record| serde_json::to_string(&record).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(directory.join("journal.jsonl"), text).unwrap();

        let journal = SessionJournal::new(temp.path().to_path_buf());
        let mut restored = journal.latest_session(session_id).unwrap().unwrap();
        assert_eq!(restored.room_id, "123");
        assert_eq!(restored.metrics.total_event_count, 1);
        let duplicate = serde_json::from_value::<DanmuEvent>(event).unwrap();
        assert!(!restored.ingest(duplicate));
    }
}
