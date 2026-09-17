use crate::domain::{DanmuEvent, DanmuEventKind};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
};

pub(crate) struct History {
    pub room: String,
    connection: Connection,
    data_directory: PathBuf,
}
impl History {
    pub fn open(parent: &Path, room: &str) -> Result<Self> {
        ensure!(
            !room.is_empty()
                && room.len() <= 64
                && room
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "无效历史房间标识"
        );
        let directory = std::path::absolute(parent)?;
        crate::persistence::check_path(&directory)?;
        std::fs::create_dir_all(&directory)?;
        let data_directory = directory.join("history").join(room);
        crate::persistence::check_path(&data_directory)?;
        crate::bridge::wire::private_root(&directory.join("history"))?;
        crate::bridge::wire::private_root(&data_directory)?;
        let path = data_directory.join("history.sqlite");
        for suffix in ["", "-wal", "-shm", "-journal"] {
            crate::persistence::check_path(
                &data_directory.join(format!("history.sqlite{suffix}")),
            )?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        ensure!(
            connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))? == "ok",
            "历史索引损坏；保留原件，停止写入"
        );
        connection.execute_batch("PRAGMA journal_mode=WAL;")?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let legacy: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('events') WHERE name='id' AND pk>0)",
            [],
            |row| row.get(0),
        )?;
        if legacy {
            // Replace FTS and its rowid references atomically with the events table.
            transaction.execute_batch(
                "DROP TRIGGER IF EXISTS events_insert;
                DROP TRIGGER IF EXISTS events_delete;
                DROP TRIGGER IF EXISTS events_update;
                DROP TABLE IF EXISTS search;
                ALTER TABLE events RENAME TO events_legacy;",
            )?;
        }
        transaction.execute_batch("CREATE TABLE IF NOT EXISTS events(identity BLOB NOT NULL UNIQUE, id TEXT NOT NULL, timestamp INTEGER NOT NULL, author_id TEXT, username TEXT, content TEXT NOT NULL, terms TEXT NOT NULL);")?;
        if legacy {
            let mut source = transaction.prepare(
                "SELECT rowid,id,timestamp,author_id,username,content,terms FROM events_legacy",
            )?;
            let mut rows = source.query([])?;
            let mut insert = transaction.prepare("INSERT INTO events(rowid,identity,id,timestamp,author_id,username,content,terms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
            while let Some(source) = rows.next()? {
                let entry = row(source)?;
                let identity = identity(
                    &entry.1,
                    entry.2,
                    entry.3.as_deref(),
                    entry.4.as_deref(),
                    &entry.5,
                )?;
                insert.execute(params![
                    entry.0,
                    identity,
                    entry.1,
                    entry.2,
                    entry.3,
                    entry.4,
                    entry.5,
                    source.get::<_, String>(6)?,
                ])?;
            }
        }
        if legacy {
            transaction.execute_batch("DROP TABLE events_legacy;")?;
        }
        transaction.execute_batch("CREATE INDEX IF NOT EXISTS events_author_time ON events(author_id,timestamp);
            CREATE VIRTUAL TABLE IF NOT EXISTS search USING fts5(terms, content='events', content_rowid='rowid');
            CREATE TRIGGER IF NOT EXISTS events_insert AFTER INSERT ON events BEGIN INSERT INTO search(rowid,terms) VALUES(new.rowid,new.terms); END;
            CREATE TRIGGER IF NOT EXISTS events_delete AFTER DELETE ON events BEGIN INSERT INTO search(search,rowid,terms) VALUES('delete',old.rowid,old.terms); END;
            CREATE TRIGGER IF NOT EXISTS events_update AFTER UPDATE ON events BEGIN INSERT INTO search(search,rowid,terms) VALUES('delete',old.rowid,old.terms); INSERT INTO search(rowid,terms) VALUES(new.rowid,new.terms); END;
            CREATE TABLE IF NOT EXISTS journal_imports(path BLOB PRIMARY KEY, bytes INTEGER NOT NULL, digest BLOB NOT NULL);
            DROP TABLE IF EXISTS imports;")?;
        if legacy {
            transaction.execute_batch("INSERT INTO search(search) VALUES('rebuild');")?;
        }
        transaction.commit()?;
        Ok(Self {
            room: room.into(),
            connection,
            data_directory,
        })
    }
    pub fn is_at(&self, parent: &Path) -> bool {
        self.data_directory == parent.join("history").join(&self.room)
    }
    pub fn migrate_directory(source: &Path, target: &Path) -> Result<()> {
        let directory = source.join("history");
        crate::persistence::check_path(&directory)?;
        if !directory.try_exists()? {
            return Ok(());
        }
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            ensure!(
                entry.file_type()?.is_dir(),
                "历史房间目录含链接或特殊文件；原件保留"
            );
            let room = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("历史房间标识不是UTF-8"))?;
            Self::open(target, &room)?.import_database(source)?;
        }
        Ok(())
    }

    /// Read a SQLite snapshot, including committed WAL pages. Never copy a live database.
    pub fn import_database(&self, parent: &Path) -> Result<()> {
        let path = parent
            .join("history")
            .join(&self.room)
            .join("history.sqlite");
        crate::persistence::check_path(&path)?;
        if !path.try_exists()? || self.is_at(parent) {
            return Ok(());
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            crate::persistence::check_path(
                &path.with_file_name(format!("history.sqlite{suffix}")),
            )?;
        }
        let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source.busy_timeout(std::time::Duration::from_secs(2))?;
        let snapshot = source.unchecked_transaction()?;
        ensure!(
            snapshot.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))? == "ok",
            "旧历史索引损坏；两边原件保留"
        );
        let transaction = self.connection.unchecked_transaction()?;
        let mut statement =
            snapshot.prepare("SELECT id,timestamp,author_id,username,content FROM events")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            self.insert(
                &row.get::<_, String>(0)?,
                row.get(1)?,
                row.get::<_, Option<String>>(2)?.as_deref(),
                row.get::<_, Option<String>>(3)?.as_deref(),
                &row.get::<_, String>(4)?,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn record(&self, event: &DanmuEvent) -> Result<()> {
        if event.kind != DanmuEventKind::Danmu {
            return Ok(());
        }
        self.insert(
            &event.id,
            event.timestamp.timestamp(),
            event.author_id.as_deref(),
            event.username.as_deref(),
            &event.content,
        )
    }
    pub(crate) fn record_many<'a>(
        &self,
        events: impl IntoIterator<Item = &'a DanmuEvent>,
    ) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        for event in events {
            self.record(event)?;
        }
        transaction.commit()?;
        Ok(())
    }
    fn insert(
        &self,
        id: &str,
        timestamp: i64,
        author_id: Option<&str>,
        username: Option<&str>,
        content: &str,
    ) -> Result<()> {
        let terms = tokens(content).into_iter().collect::<Vec<_>>().join(" ");
        let identity = identity(id, timestamp, author_id, username, content)?;
        self.connection.prepare_cached("INSERT INTO events(identity,id,timestamp,author_id,username,content,terms) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(identity) DO NOTHING")?
            .execute(params![identity, id, timestamp, author_id, username, content, terms])?;
        Ok(())
    }
    pub fn import_journal(&self, path: &Path) -> Result<()> {
        crate::persistence::check_path(path)?;
        let file = File::open(path)?;
        let bytes = i64::try_from(file.metadata()?.len())?;
        let key = path.as_os_str().as_encoded_bytes();
        let previous: Option<(i64, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT bytes,digest FROM journal_imports WHERE path=?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let offset = previous.as_ref().map_or(0, |(n, _)| *n);
        ensure!(
            offset >= 0 && offset <= bytes,
            "历史归档已截断；保留索引和归档，停止导入"
        );
        // Journals are append-only. Restrict this import to the prefix that existed
        // when the file was opened so writers remain unblocked and a busy journal
        // cannot make the scan chase a moving EOF.
        let mut reader = BufReader::new(file.take(bytes as u64));
        let mut digest = Sha256::new();
        std::io::copy(&mut (&mut reader).take(offset as u64), &mut digest)?;
        if let Some((_, expected)) = &previous {
            ensure!(
                &digest.clone().finalize()[..] == expected,
                "历史归档已修改；保留两边，停止导入"
            );
            if offset == bytes {
                return Ok(());
            }
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Record<'a> {
            #[serde(default)]
            schema_version: Option<u32>,
            kind: Kind,
            #[serde(borrow)]
            payload: &'a serde_json::value::RawValue,
        }
        #[derive(Deserialize, PartialEq)]
        #[serde(rename_all = "camelCase")]
        enum Kind {
            SessionStarted,
            EventReceived,
            #[serde(other)]
            Other,
        }
        #[derive(Deserialize)]
        struct LegacyRoom {
            #[serde(rename = "roomID")]
            id: String,
        }
        #[derive(Deserialize)]
        struct LegacyPayload {
            event: LegacyEvent,
        }
        #[derive(Deserialize)]
        struct LegacyEvent {
            id: String,
            kind: DanmuEventKind,
            timestamp: chrono::DateTime<chrono::Utc>,
            username: Option<String>,
            #[serde(rename = "authorID")]
            author_id: Option<String>,
            content: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Room {
            room_id: String,
        }
        let offset = offset as u64;
        let mut completed = offset;
        let mut line = Vec::new();
        let mut belongs = offset > 0;
        let transaction = self.connection.unchecked_transaction()?;
        loop {
            line.clear();
            let count = (&mut reader)
                .take(8 * 1024 * 1024 + 1)
                .read_until(b'\n', &mut line)?;
            if count == 0 {
                break;
            }
            ensure!(count <= 8 * 1024 * 1024, "旧弹幕归档单条过大，未完成索引");
            if line.last() != Some(&b'\n') {
                break;
            } // A crash can leave an unfinished final record.
            let record: Record =
                serde_json::from_slice(&line).context("历史归档损坏，未完成索引")?;
            completed += count as u64;
            digest.update(&line);
            let legacy = match record.schema_version {
                None => false,
                Some(1) => true,
                Some(version) => anyhow::bail!("不支持的历史归档版本：{version}"),
            };
            if record.kind == Kind::SessionStarted {
                let room = if legacy {
                    serde_json::from_str::<LegacyRoom>(record.payload.get())?.id
                } else {
                    serde_json::from_str::<Room>(record.payload.get())?.room_id
                };
                if room != self.room {
                    return Ok(());
                }
                belongs = true;
            } else if belongs && record.kind == Kind::EventReceived {
                if legacy {
                    let event = serde_json::from_str::<LegacyPayload>(record.payload.get())?.event;
                    if event.kind == DanmuEventKind::Danmu {
                        self.insert(
                            &event.id,
                            event.timestamp.timestamp(),
                            event.author_id.as_deref(),
                            event.username.as_deref(),
                            &event.content,
                        )?;
                    }
                } else {
                    self.record(&serde_json::from_str::<DanmuEvent>(record.payload.get())?)?;
                }
            }
        }
        if belongs {
            transaction.execute("INSERT INTO journal_imports(path,bytes,digest) VALUES(?1,?2,?3) ON CONFLICT(path) DO UPDATE SET bytes=excluded.bytes,digest=excluded.digest", params![key,completed,digest.finalize().to_vec()])?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn environment(&self, facts: &Value) -> Result<()> {
        crate::persistence::check_path(&self.data_directory.join("environment.json"))?;
        let mut file = tempfile::NamedTempFile::new_in(&self.data_directory)?;
        serde_json::to_writer_pretty(file.as_file_mut(), facts)?;
        file.write_all(b"\n")?;
        file.persist(self.data_directory.join("environment.json"))?;
        Ok(())
    }
    /// Same-room, exact-UID evidence; relevance first, then recent interactions.
    pub fn viewer_history<'a>(
        &self,
        viewers: impl Iterator<Item = (&'a str, &'a str)>,
        before: i64,
        budget: usize,
    ) -> Result<Vec<Value>> {
        if budget < 512 {
            return Ok(Vec::new());
        }
        let mut authors = BTreeSet::new();
        let mut groups = Vec::new();
        for (uid, question) in viewers.take(8) {
            if uid.trim().is_empty() || matches!(uid, "0" | "local-host") || !authors.insert(uid) {
                continue;
            }
            let mut entries = Vec::new();
            let query = tokens(question)
                .into_iter()
                .take(24)
                .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" OR ");
            if !query.is_empty() {
                let matched = self.connection.prepare_cached(
                    "SELECT e.rowid,e.id,e.timestamp,e.author_id,e.username,e.content FROM search JOIN events e ON e.rowid=search.rowid WHERE search MATCH ?1 AND e.author_id=?2 AND e.timestamp<?3 ORDER BY bm25(search),e.timestamp DESC,e.rowid DESC LIMIT 1"
                )?.query_row(params![query, uid, before], row).optional()?;
                entries.extend(matched);
            }
            let mut recent = self.connection.prepare_cached(
                "SELECT rowid,id,timestamp,author_id,username,content FROM events WHERE author_id=?1 AND timestamp<?2 ORDER BY timestamp DESC,rowid DESC LIMIT 3"
            )?;
            for entry in recent.query_map(params![uid, before], row)? {
                let entry = entry?;
                if !entries.iter().any(|existing| existing.0 == entry.0) {
                    entries.push(entry);
                }
                if entries.len() == 3 {
                    break;
                }
            }
            groups.push(entries);
        }
        let mut output = Vec::new();
        let mut seen = BTreeSet::new();
        let mut used = 2;
        // Give each viewer one slot before adding another viewer's older interactions.
        for depth in 0..3 {
            for entries in &groups {
                if let Some(entry) = entries.get(depth) {
                    push(
                        &mut output,
                        &mut seen,
                        &mut used,
                        budget,
                        entry,
                        "",
                        "same_viewer_history",
                    );
                }
            }
        }
        Ok(output)
    }

    pub fn search<'a>(
        &self,
        queries: impl Iterator<Item = &'a str>,
        broadcaster: &str,
        excluded: &[String],
        budget: usize,
    ) -> Result<Vec<Value>> {
        if budget < 512 {
            return Ok(Vec::new());
        }
        let terms: BTreeSet<_> = queries.flat_map(tokens).take(48).collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let query = terms
            .iter()
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut statement = self.connection.prepare_cached("SELECT e.rowid,e.id,e.timestamp,e.author_id,e.username,e.content FROM search JOIN events e ON e.rowid=search.rowid WHERE search MATCH ?1 ORDER BY bm25(search)*(CASE WHEN e.author_id=?2 AND ?2<>'' THEN 2.0 ELSE 1.0 END),e.timestamp DESC LIMIT 8")?;
        let hits = statement
            .query_map(params![query, broadcaster], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut output = Vec::new();
        let mut seen = BTreeSet::new();
        let mut used = 2; // JSON array delimiters.
        for hit in hits {
            if excluded.contains(&hit.1) {
                continue;
            }
            let stamp = hit.2;
            let host = hit.3.as_deref() == Some(broadcaster) && !broadcaster.is_empty();
            push(
                &mut output,
                &mut seen,
                &mut used,
                budget,
                &hit,
                broadcaster,
                "matched",
            );
            if !host && !broadcaster.is_empty() {
                let mut nearby = self.connection.prepare_cached("SELECT rowid,id,timestamp,author_id,username,content FROM events WHERE author_id=?1 AND timestamp>=?2 AND timestamp<=?3 ORDER BY timestamp LIMIT 2")?;
                for next in
                    nearby.query_map(params![broadcaster, stamp, stamp.saturating_add(120)], row)?
                {
                    let next = next?;
                    if !excluded.contains(&next.1) {
                        push(
                            &mut output,
                            &mut seen,
                            &mut used,
                            budget,
                            &next,
                            broadcaster,
                            "nearby_not_proven_answer",
                        );
                    }
                }
            }
            if output.len() >= 8 {
                break;
            }
        }
        Ok(output)
    }
}
// Platform IDs are source metadata, not storage keys. JSON preserves field boundaries
// and distinguishes absent author/name values from empty strings.
fn identity(
    id: &str,
    timestamp: i64,
    author_id: Option<&str>,
    username: Option<&str>,
    content: &str,
) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    serde_json::to_writer(&mut digest, &(id, timestamp, author_id, username, content))?;
    Ok(digest.finalize().into())
}
type Entry = (i64, String, i64, Option<String>, Option<String>, String);
fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}
fn push(
    output: &mut Vec<Value>,
    seen: &mut BTreeSet<i64>,
    used: &mut usize,
    budget: usize,
    entry: &Entry,
    broadcaster: &str,
    relationship: &str,
) {
    if output.len() >= 8 || seen.contains(&entry.0) {
        return;
    }
    let value = json!({"id":entry.1,"timestamp":entry.2,"author_id":entry.3,"username":entry.4,"content":entry.5.chars().take(240).collect::<String>(),"broadcaster_account":!broadcaster.is_empty() && entry.3.as_deref()==Some(broadcaster),"relationship":relationship});
    let bytes =
        crate::workspace::encoded_len(&value).saturating_add(usize::from(!output.is_empty()));
    if bytes > budget.saturating_sub(*used) {
        return;
    }
    *used += bytes;
    seen.insert(entry.0);
    output.push(value);
}
fn tokens(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut latin = String::new();
    let mut previous = None;
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '_' {
            latin.push(c);
            previous = None;
        } else {
            if latin.len() > 1 {
                out.insert(std::mem::take(&mut latin));
            } else {
                latin.clear();
            }
            if ('\u{3400}'..='\u{9fff}').contains(&c) {
                if let Some(p) = previous {
                    out.insert(format!("{p}{c}"));
                }
                previous = Some(c);
            } else {
                previous = None;
            }
        }
    }
    if latin.len() > 1 {
        out.insert(latin);
    }
    for stop in [
        "什么", "一个", "这个", "那个", "是否", "我们", "你们", "现在", "可以",
    ] {
        out.remove(stop);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(id: &str, uid: &str, seconds: i64, content: &str) -> DanmuEvent {
        let mut event = DanmuEvent::new(DanmuEventKind::Danmu, content);
        event.id = id.into();
        event.author_id = Some(uid.into());
        event.username = Some("同名主播".into());
        event.timestamp = chrono::DateTime::from_timestamp(seconds, 0).unwrap();
        event
    }

    #[test]
    fn viewer_memory_reopens_with_uid_room_time_and_relevance_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let history = History::open(root.path(), "123").unwrap();
        history
            .record(&event("reused", "42", 1000, "Flink订单窗口乱序的问题"))
            .unwrap();
        for i in 0..5 {
            history
                .record(&event("reused", "42", 2000 + i, "最近聊过摄影"))
                .unwrap();
        }
        history
            .record(&event("namesake", "43", 2005, "Flink是同名另一位的问题"))
            .unwrap();
        history
            .record(&event("current", "42", 3000, "当前批次不能作为过去记忆"))
            .unwrap();
        history
            .record(&event("future", "42", 4000, "未来事件不能作为过去记忆"))
            .unwrap();
        drop(history);
        let history = History::open(root.path(), "123").unwrap();
        let hits = history
            .viewer_history([("42", "Flink窗口还记得吗")].into_iter(), 3000, 3072)
            .unwrap();
        assert_eq!(hits[0]["content"], "Flink订单窗口乱序的问题");
        assert!(
            hits.iter()
                .all(|hit| hit["author_id"] == "42" && hit["timestamp"].as_i64().unwrap() < 3000)
        );
        assert_eq!(
            hits.iter()
                .map(|hit| hit["timestamp"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            [1000, 2004, 2003]
        );
        assert!(
            history
                .viewer_history(
                    [("44", "Flink"), ("0", "Flink"), ("", "Flink")].into_iter(),
                    3000,
                    3072
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            History::open(root.path(), "456")
                .unwrap()
                .viewer_history([("42", "Flink")].into_iter(), 3000, 3072)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn viewer_memory_shares_slots_without_exceeding_encoded_budget() {
        let root = tempfile::tempdir().unwrap();
        let history = History::open(root.path(), "123").unwrap();
        let viewers = (1..=8).map(|uid| uid.to_string()).collect::<Vec<_>>();
        for uid in &viewers {
            for stamp in 1..=3 {
                history
                    .record(&event("shared", uid, stamp, "短互动"))
                    .unwrap();
            }
        }
        let hits = history
            .viewer_history(viewers.iter().map(|uid| (uid.as_str(), "")), 4, 3072)
            .unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| hit["author_id"].as_str().unwrap())
                .collect::<BTreeSet<_>>(),
            viewers.iter().map(String::as_str).collect()
        );
        assert!(crate::workspace::encoded_len(&Value::Array(hits)) <= 3072);
        let bounded = history
            .viewer_history(viewers.iter().map(|uid| (uid.as_str(), "")), 4, 512)
            .unwrap();
        assert!(crate::workspace::encoded_len(&Value::Array(bounded)) <= 512);
    }

    #[test]
    fn record_many_is_atomic_and_skips_non_danmu_events() {
        let root = tempfile::tempdir().unwrap();
        let history = History::open(root.path(), "123").unwrap();
        history
            .connection
            .execute_batch(
                "CREATE TEMP TRIGGER reject_batch BEFORE INSERT ON events
                 WHEN new.id='reject' BEGIN SELECT RAISE(ABORT, 'reject'); END;",
            )
            .unwrap();
        let events = [
            event("first", "viewer", 1000, "批量事务第一条"),
            event("reject", "viewer", 1001, "批量事务拒绝条"),
            event("last", "viewer", 1002, "批量事务最后条"),
        ];
        assert!(history.record_many(events.iter()).is_err());
        assert_eq!(
            history
                .connection
                .query_row("SELECT count(*) FROM events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );

        history
            .connection
            .execute_batch("DROP TRIGGER reject_batch")
            .unwrap();
        let mut ignored = event("ignored", "viewer", 1003, "不应进入历史");
        ignored.kind = DanmuEventKind::System;
        history
            .record_many([&events[0], &ignored, &events[2]])
            .unwrap();
        assert_eq!(
            history
                .connection
                .query_row("SELECT count(*) FROM events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[cfg(unix)]
    #[test]
    fn journal_import_does_not_wait_for_the_source_file_lock() {
        use std::{sync::mpsc, time::Duration};

        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join("journal.jsonl");
        std::fs::write(
            &journal,
            format!(
                "{}\n{}\n",
                json!({"kind":"sessionStarted","payload":{"roomId":"123"}}),
                json!({"kind":"eventReceived","payload":event("saved", "host", 1, "锁外固定前缀")})
            ),
        )
        .unwrap();
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&journal)
            .unwrap();
        fs2::FileExt::lock_exclusive(&locked).unwrap();

        let root = root.path().to_owned();
        let path = journal.clone();
        let (sender, receiver) = mpsc::channel();
        let importer = std::thread::spawn(move || {
            let result =
                History::open(&root, "123").and_then(|history| history.import_journal(&path));
            sender.send(result).unwrap();
        });
        let result = receiver.recv_timeout(Duration::from_secs(2));
        fs2::FileExt::unlock(&locked).unwrap();
        let result = result.expect("journal import waited for an advisory source lock");
        result.unwrap();
        importer.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn journal_import_propagates_a_mandatory_source_lock_and_recovers() {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join("journal.jsonl");
        std::fs::write(
            &journal,
            format!(
                "{}\n{}\n",
                json!({"kind":"sessionStarted","payload":{"roomId":"123"}}),
                json!({"kind":"eventReceived","payload":event("saved", "host", 1, "锁后恢复导入")})
            ),
        )
        .unwrap();
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&journal)
            .unwrap();
        fs2::FileExt::lock_exclusive(&locked).unwrap();

        let history = History::open(root.path(), "123").unwrap();
        let error = history.import_journal(&journal).unwrap_err();
        assert!(error.downcast_ref::<std::io::Error>().is_some());

        fs2::FileExt::unlock(&locked).unwrap();
        history.import_journal(&journal).unwrap();
        let hits = history
            .search(["锁后恢复"].into_iter(), "", &[], 4096)
            .unwrap();
        assert!(hits.iter().any(|hit| hit["id"] == "saved"));
    }
    #[test]
    fn reopened_history_keeps_room_and_uid_boundaries_and_retrieves_nearby_host_context() {
        let root = tempfile::tempdir().unwrap();
        let history = History::open(root.path(), "123").unwrap();
        history
            .record(&event("question", "impostor", 1000, "检索历史弹幕怎么做"))
            .unwrap();
        history
            .record(&event(
                "answer",
                "host",
                1010,
                "按相关性匹配，不保证问答对应",
            ))
            .unwrap();
        drop(history);
        let history = History::open(root.path(), "123").unwrap();
        let hits = history
            .search(["检索"].into_iter(), "host", &[], 4096)
            .unwrap();
        assert!(
            hits.iter()
                .any(|v| v["id"] == "question" && v["broadcaster_account"] == false)
        );
        assert!(hits.iter().any(|v| v["id"] == "answer"
            && v["broadcaster_account"] == true
            && v["relationship"] == "nearby_not_proven_answer"));
        assert!(
            History::open(root.path(), "456")
                .unwrap()
                .search(["检索"].into_iter(), "host", &[], 4096)
                .unwrap()
                .is_empty()
        );
        assert!(
            history
                .search(["检索"].into_iter(), "host", &["question".into()], 4096)
                .unwrap()
                .is_empty()
        );
        history
            .record(&event("host-match", "host", 5000, "向量索引检索"))
            .unwrap();
        history
            .record(&event("viewer-match", "impostor", 5000, "向量索引检索"))
            .unwrap();
        let hits = history
            .search(["向量索引"].into_iter(), "host", &[], 4096)
            .unwrap();
        assert!(
            hits.iter().position(|v| v["id"] == "host-match").unwrap()
                < hits.iter().position(|v| v["id"] == "viewer-match").unwrap()
        );
    }
    #[test]
    fn legacy_import_resumes_complete_records_without_importing_other_room_or_private_commands() {
        let root = tempfile::tempdir().unwrap();
        let history = History::open(root.path(), "123").unwrap();
        let journal = root.path().join("journal.jsonl");
        let record = |kind, payload| format!("{}\n", json!({"kind":kind,"payload":payload}));
        let tail = record(
            "eventReceived",
            json!(event("second", "host", 1010, "历史第二条")),
        );
        let mut text = record("sessionStarted", json!({"roomId":"123"}));
        text += &record(
            "eventReceived",
            json!(event("first", "viewer", 1000, "历史第一条")),
        );
        text += &record("unhandledCommand", json!({"secret":"不应索引的私密指令"}));
        text += &tail[..tail.len() - 1];
        std::fs::write(&journal, text).unwrap();
        history.import_journal(&journal).unwrap();
        assert!(
            !history
                .search(["历史"].into_iter(), "host", &[], 4096)
                .unwrap()
                .iter()
                .any(|v| v["id"] == "second")
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        history.import_journal(&journal).unwrap();
        assert!(
            history
                .search(["历史"].into_iter(), "host", &[], 4096)
                .unwrap()
                .iter()
                .any(|v| v["id"] == "second")
        );
        assert!(
            history
                .search(["私密指令"].into_iter(), "host", &[], 4096)
                .unwrap()
                .is_empty()
        );
        let other = root.path().join("other.jsonl");
        std::fs::write(
            &other,
            record("sessionStarted", json!({"roomId":"456"}))
                + &record(
                    "eventReceived",
                    json!(event("foreign", "host", 1000, "历史外部房间")),
                ),
        )
        .unwrap();
        history.import_journal(&other).unwrap();
        assert!(
            !history
                .search(["历史"].into_iter(), "host", &[], 4096)
                .unwrap()
                .iter()
                .any(|v| v["id"] == "foreign")
        );
    }

    #[test]
    fn imports_swift_v1_and_current_journal_writer_without_crossing_rooms() {
        use crate::{domain::DanmuSession, persistence::SessionJournal};
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let journal = SessionJournal::new(sessions.clone());
        let session = DanmuSession::new("123");
        let directory = sessions.join(&session.id);
        std::fs::create_dir_all(&directory).unwrap();
        let legacy_event = json!({"id":"swift-event","kind":"danmu","timestamp":"2026-09-09T00:00:00Z","username":"主播","authorID":"host","content":"检索旧版归档","platformEventID":null});
        let legacy = [
            json!({"schemaVersion":1,"kind":"sessionStarted","payload":{"roomID":"123"}}),
            json!({"schemaVersion":1,"kind":"featuredChanged","payload":{"featuredEventID":"swift-event"}}),
            json!({"schemaVersion":1,"kind":"eventReceived","payload":{"event":legacy_event}}),
            json!({"schemaVersion":1,"kind":"sessionResumed","payload":{}}),
        ].into_iter().map(|record| format!("{record}\n")).collect::<String>();
        let path = directory.join("journal.jsonl");
        std::fs::write(&path, &legacy).unwrap();
        let history = History::open(root.path(), "123").unwrap();
        journal.index_history(&history).unwrap();
        let hits = history
            .search(["检索"].into_iter(), "host", &[], 4096)
            .unwrap();
        assert!(
            hits.iter()
                .any(|v| v["id"] == "swift-event" && v["broadcaster_account"] == true)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy);
        let other_history = History::open(root.path(), "456").unwrap();
        journal.index_history(&other_history).unwrap();
        assert!(
            other_history
                .search(["检索"].into_iter(), "host", &[], 4096)
                .unwrap()
                .is_empty()
        );
        journal.start(&session).unwrap();
        journal
            .event(
                &session,
                &event("rust-event", "viewer", 1100, "检索新版归档"),
            )
            .unwrap();
        let foreign = DanmuSession::new("456");
        journal.start(&foreign).unwrap();
        journal
            .event(
                &foreign,
                &event("foreign-event", "host", 1200, "检索其他房间"),
            )
            .unwrap();
        journal.index_history(&history).unwrap();
        drop(history);
        let reopened = History::open(root.path(), "123").unwrap();
        let hits = reopened
            .search(["检索"].into_iter(), "host", &[], 4096)
            .unwrap();
        assert!(hits.iter().any(|v| v["id"] == "swift-event"));
        assert!(hits.iter().any(|v| v["id"] == "rust-event"));
        assert!(!hits.iter().any(|v| v["id"] == "foreign-event"));
    }

    #[test]
    fn workspace_migration_reads_live_wal_and_preserves_conflicting_versions() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("private");
        let workspace = crate::workspace::managed_root(&root.path().join("workspace")).unwrap();
        let source = History::open(&old, "123").unwrap();
        source
            .connection
            .execute_batch("PRAGMA wal_autocheckpoint=0")
            .unwrap();
        source
            .record(&event("old", "host", 1000, "持久检索旧索引"))
            .unwrap();
        assert!(
            old.join("history/123/history.sqlite-wal")
                .metadata()
                .unwrap()
                .len()
                > 0
        );
        let target = History::open(&workspace, "123").unwrap();
        target.import_database(&old).unwrap();
        source
            .record(&event("late", "host", 2000, "持久检索后续WAL"))
            .unwrap();
        target.import_database(&old).unwrap();
        target.import_database(&old).unwrap();
        drop(target);
        let target = History::open(&workspace, "123").unwrap();
        let hits = target
            .search(["持久检索"].into_iter(), "host", &[], 4096)
            .unwrap();
        assert!(hits.iter().any(|row| row["id"] == "old"));
        assert!(hits.iter().any(|row| row["id"] == "late"));
        assert!(
            History::open(&workspace, "456")
                .unwrap()
                .search(["持久检索"].into_iter(), "host", &[], 4096)
                .unwrap()
                .is_empty()
        );

        source
            .record(&event("batch", "host", 3000, "冲突批次独有内容"))
            .unwrap();
        source
            .record(&event("collision", "host", 4000, "旧库原件"))
            .unwrap();
        target
            .record(&event("collision", "host", 4000, "目标原件"))
            .unwrap();
        source
            .record(&event("after", "host", 5000, "冲突后续消息"))
            .unwrap();
        target.import_database(&old).unwrap();
        target.import_database(&old).unwrap();
        drop(target);
        let target = History::open(&workspace, "123").unwrap();
        target.import_database(&old).unwrap();
        let hits = target
            .search(["原件"].into_iter(), "host", &[], 4096)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(
            hits.iter()
                .any(|v| v["id"] == "collision" && v["content"] == "旧库原件")
        );
        assert!(
            hits.iter()
                .any(|v| v["id"] == "collision" && v["content"] == "目标原件")
        );
        let hits = target
            .search(["冲突"].into_iter(), "host", &[], 4096)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|v| v["id"] == "batch"));
        assert!(hits.iter().any(|v| v["id"] == "after"));
        assert_eq!(
            source
                .search(["旧库原件"].into_iter(), "host", &[], 4096)
                .unwrap()[0]["content"],
            "旧库原件"
        );
    }

    #[test]
    fn changed_or_truncated_import_cannot_rebind_a_room_or_overwrite_history() {
        let root = tempfile::tempdir().unwrap();
        let history = History::open(root.path(), "123").unwrap();
        let path = root.path().join("journal.jsonl");
        let body = format!(
            "{}\n{}\n",
            json!({"kind":"sessionStarted","payload":{"roomId":"123"}}),
            json!({"kind":"eventReceived","payload":event("saved", "host", 1, "原始检索正文")})
        );
        std::fs::write(&path, &body).unwrap();
        history.import_journal(&path).unwrap();
        std::fs::write(&path, body.replace("123", "456")).unwrap();
        assert!(history.import_journal(&path).is_err());
        std::fs::write(&path, "").unwrap();
        assert!(history.import_journal(&path).is_err());
        assert_eq!(
            history
                .search(["原始检索"].into_iter(), "host", &[], 4096)
                .unwrap()[0]["id"],
            "saved"
        );
    }
    #[test]
    fn legacy_empty_id_migration_preserves_versions_journal_progress_and_fts() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("history/123");
        crate::bridge::wire::private_root(&root.path().join("history")).unwrap();
        crate::bridge::wire::private_root(&directory).unwrap();
        let legacy = Connection::open(directory.join("history.sqlite")).unwrap();
        legacy.execute_batch("PRAGMA journal_mode=WAL;
            PRAGMA wal_autocheckpoint=0;
            CREATE TABLE events(id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL, author_id TEXT, username TEXT, content TEXT NOT NULL, terms TEXT NOT NULL);
            CREATE INDEX events_author_time ON events(author_id,timestamp);
            CREATE VIRTUAL TABLE search USING fts5(terms, content='events', content_rowid='rowid');
            CREATE TRIGGER events_insert AFTER INSERT ON events BEGIN INSERT INTO search(rowid,terms) VALUES(new.rowid,new.terms); END;
            CREATE TRIGGER events_delete AFTER DELETE ON events BEGIN INSERT INTO search(search,rowid,terms) VALUES('delete',old.rowid,old.terms); END;
            CREATE TRIGGER events_update AFTER UPDATE ON events BEGIN INSERT INTO search(search,rowid,terms) VALUES('delete',old.rowid,old.terms); INSERT INTO search(rowid,terms) VALUES(new.rowid,new.terms); END;
            CREATE TABLE journal_imports(path BLOB PRIMARY KEY, bytes INTEGER NOT NULL, digest BLOB NOT NULL);
            INSERT INTO events VALUES('',1000,'host','同名主播','legacy original','legacy original');").unwrap();
        let journal = root.path().join("journal.jsonl");
        let mut body = format!(
            "{}\n{}\n",
            json!({"kind":"sessionStarted","payload":{"roomId":"123"}}),
            json!({"kind":"eventReceived","payload":event("", "host", 1000, "legacy original")}),
        );
        std::fs::write(&journal, &body).unwrap();
        legacy
            .execute(
                "INSERT INTO journal_imports VALUES(?1,?2,?3)",
                params![
                    journal.as_os_str().as_encoded_bytes(),
                    body.len() as i64,
                    &Sha256::digest(body.as_bytes())[..],
                ],
            )
            .unwrap();

        // A read-only merge also accepts the old schema while its committed rows are in WAL.
        let merged_root = tempfile::tempdir().unwrap();
        let merged = History::open(merged_root.path(), "123").unwrap();
        merged
            .record(&event("", "host", 1000, "legacy target"))
            .unwrap();
        merged.import_database(root.path()).unwrap();
        merged.import_database(root.path()).unwrap();
        let hits = merged
            .search(["legacy"].into_iter(), "", &[], 8192)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(
            hits.iter()
                .any(|v| v["content"] == "legacy original" && v["id"] == "")
        );
        assert!(
            hits.iter()
                .any(|v| v["content"] == "legacy target" && v["id"] == "")
        );
        drop(legacy);

        let history = History::open(root.path(), "123").unwrap();
        std::fs::write(&journal, body.replace("legacy original", "legacy modified")).unwrap();
        assert!(history.import_journal(&journal).is_err());
        std::fs::write(&journal, &body).unwrap();
        history
            .record(&event("", "host", 1000, "legacy original"))
            .unwrap();
        for next in [
            event("", "host", 1000, "legacy variant"),
            event("later", "viewer", 1100, "legacy subsequent"),
        ] {
            body += &format!("{}\n", json!({"kind":"eventReceived","payload":next}));
        }
        std::fs::write(&journal, &body).unwrap();
        history.import_journal(&journal).unwrap();
        let duplicate = root.path().join("duplicate.jsonl");
        std::fs::write(&duplicate, &body).unwrap();
        history.import_journal(&duplicate).unwrap();
        drop(history);
        let history = History::open(root.path(), "123").unwrap();
        history.import_journal(&journal).unwrap();
        history.import_journal(&duplicate).unwrap();
        let hits = history
            .search(["legacy"].into_iter(), "host", &[], 8192)
            .unwrap();
        assert_eq!(hits.len(), 3);
        for (id, content) in [
            ("", "legacy original"),
            ("", "legacy variant"),
            ("later", "legacy subsequent"),
        ] {
            assert!(
                hits.iter()
                    .any(|v| v["id"] == id && v["content"] == content)
            );
        }
        let hits = history
            .search(["legacy"].into_iter(), "host", &["".into()], 8192)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], "later");
        assert_eq!(std::fs::read_to_string(&journal).unwrap(), body);
        // Progress must survive migration: an already imported prefix cannot be rewritten.
        std::fs::write(&journal, body.replace("legacy original", "legacy modified")).unwrap();
        assert!(history.import_journal(&journal).is_err());

        history
            .connection
            .execute(
                "UPDATE events SET terms='updated' WHERE content='legacy original'",
                [],
            )
            .unwrap();
        assert_eq!(
            history
                .search(["updated"].into_iter(), "", &[], 8192)
                .unwrap()[0]["content"],
            "legacy original"
        );
        assert_eq!(
            history
                .search(["legacy"].into_iter(), "", &[], 8192)
                .unwrap()
                .len(),
            2
        );
        history
            .connection
            .execute("DELETE FROM events WHERE content='legacy original'", [])
            .unwrap();
        assert!(
            history
                .search(["updated"].into_iter(), "", &[], 8192)
                .unwrap()
                .is_empty()
        );
        history
            .connection
            .execute(
                "INSERT INTO search(search,rank) VALUES('integrity-check',1)",
                [],
            )
            .unwrap();
    }

    #[test]
    fn identity_retains_every_tuple_variant_but_deduplicates_replays_and_nearby_hits() {
        let root = tempfile::tempdir().unwrap();
        let history = History::open(root.path(), "123").unwrap();
        let variants = [
            ("reused", 1000, None, None, "shared original"),
            ("reused", 1001, None, None, "shared original"),
            ("reused", 1000, Some(""), None, "shared original"),
            ("reused", 1000, None, Some(""), "shared original"),
            (
                "reused",
                1000,
                Some("host"),
                Some("name"),
                "shared original",
            ),
            ("reused", 1000, None, None, "shared variant"),
            ("different", 1000, None, None, "shared original"),
        ];
        for _ in 0..2 {
            for (id, stamp, author, name, content) in variants {
                history.insert(id, stamp, author, name, content).unwrap();
            }
        }
        let hits = history
            .search(["shared"].into_iter(), "host", &[], 16384)
            .unwrap();
        assert_eq!(hits.len(), variants.len());
        for (id, stamp, author, name, content) in variants {
            assert!(hits.iter().any(|v| v["id"] == id
                && v["timestamp"] == stamp
                && v["author_id"] == json!(author)
                && v["username"] == json!(name)
                && v["content"] == content));
        }
        let hits = history
            .search(["shared"].into_iter(), "host", &["reused".into()], 16384)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], "different");
    }

    #[test]
    fn damaged_imports_roll_back_without_masking_sqlite_errors() {
        let root = tempfile::tempdir().unwrap();
        let history = History::open(root.path(), "123").unwrap();
        let journal = root.path().join("journal.jsonl");
        std::fs::write(
            &journal,
            format!(
                "{}\n{}\nnot json\n",
                json!({"kind":"sessionStarted","payload":{"roomId":"123"}}),
                json!({"kind":"eventReceived","payload":event("", "host", 1, "rollback pending")}),
            ),
        )
        .unwrap();
        assert!(history.import_journal(&journal).is_err());
        assert!(
            history
                .search(["rollback"].into_iter(), "", &[], 4096)
                .unwrap()
                .is_empty()
        );
        let source = tempfile::tempdir().unwrap();
        let directory = source.path().join("history/123");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("history.sqlite"), b"not a SQLite database").unwrap();
        assert!(history.import_database(source.path()).is_err());
        history.connection.execute_batch("CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'storage failure'); END;").unwrap();
        let error = history
            .record(&event("next", "host", 2, "rollback next"))
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<rusqlite::Error>()
                .unwrap()
                .sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );
        history
            .connection
            .execute_batch("DROP TRIGGER reject_event;")
            .unwrap();
        history
            .record(&event("next", "host", 2, "rollback next"))
            .unwrap();
        assert_eq!(
            history
                .search(["rollback"].into_iter(), "", &[], 4096)
                .unwrap()[0]["id"],
            "next"
        );
    }
}
