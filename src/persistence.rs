use crate::domain::{DanmuEvent, DanmuSession, DanmuSessionEndReason};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
    AutoReply,
    AssistantStopped,
    AssistantRouting,
}

#[derive(Debug, Clone)]
pub struct SessionJournal {
    root: PathBuf,
    mirror: Arc<Mutex<Option<JournalMirror>>>,
}

pub(crate) fn serialize_exports(session: &DanmuSession) -> Result<(Vec<u8>, Vec<u8>)> {
    let snapshot = serde_json::to_vec_pretty(session)?;
    let summary = format!(
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
    )
    .into_bytes();
    Ok((snapshot, summary))
}

impl SessionJournal {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            mirror: Arc::new(Mutex::new(None)),
        }
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn index_history(&self, history: &crate::history::History) -> Result<()> {
        let mut failed = 0;
        let mut first = None;
        for session in self.session_ids()? {
            let path = self.session_directory(&session).join("journal.jsonl");
            if let Err(error) = history.import_journal(&path) {
                failed += 1;
                first.get_or_insert_with(|| format!("{}：{error:#}", path.display()));
            }
        }
        anyhow::ensure!(
            failed == 0,
            "{failed} 份归档未完成索引；其他归档已继续处理。首个错误：{}",
            first.as_deref().unwrap_or("")
        );
        Ok(())
    }

    /// The private journal remains recovery truth; workspace files are read-only maintenance copies.
    pub(crate) fn bind_workspace(&self, workspace: &Path, room: &str) -> Result<()> {
        let root = crate::workspace::managed_root(workspace)?.join("sessions");
        let mut guard = self
            .mirror
            .lock()
            .map_err(|_| anyhow::anyhow!("归档副本锁损坏"))?;
        if guard
            .as_ref()
            .is_some_and(|mirror| mirror.root == root && mirror.room.as_deref() == Some(room))
        {
            return Ok(());
        }
        *guard = self.copy_to(workspace, Some(room))?;
        Ok(())
    }

    pub(crate) fn unbind_workspace(&self) -> Result<()> {
        *self
            .mirror
            .lock()
            .map_err(|_| anyhow::anyhow!("归档副本锁损坏"))? = None;
        Ok(())
    }

    pub(crate) fn migrate_to(&self, workspace: &Path) -> Result<()> {
        self.copy_to(workspace, None)?;
        Ok(())
    }

    fn copy_to(&self, workspace: &Path, room: Option<&str>) -> Result<Option<JournalMirror>> {
        let root = crate::workspace::managed_root(workspace)?.join("sessions");
        check_path(&root)?;
        crate::bridge::wire::private_root(&root)?;
        let mut mirror = JournalMirror {
            root,
            room: room.map(str::to_owned),
            files: BTreeMap::new(),
            journals: BTreeMap::new(),
        };
        check_path(&self.root)?;
        // Distinct path spellings can identify the same archive (e.g. macOS /var).
        // Never acquire an exclusive destination lock while holding its own source lock.
        if self.root.try_exists()? && self.root.canonicalize()? == mirror.root.canonicalize()? {
            return Ok(None);
        }
        for id in self.session_ids()? {
            let source = self.session_directory(&id);
            let path = source.join("journal.jsonl");
            check_path(&path)?;
            let mut file = File::open(&path)?;
            mirror.sync_journal(&source, &mut file, None)?;
            mirror.sync_exports(&source)?;
        }
        Ok(Some(mirror))
    }

    pub fn start(&self, session: &DanmuSession) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::SessionStarted,
            serde_json::to_value(session)?,
        )
    }

    pub fn event(&self, session: &DanmuSession, event: &DanmuEvent) -> Result<()> {
        let result = self.append(
            &session.id,
            JournalKind::EventReceived,
            serde_json::to_value(event)?,
        );
        // A failed maintenance copy must not prevent the private recovery snapshot.
        let snapshot = self.snapshot(session);
        result.and(snapshot)
    }

    pub fn unhandled_command(&self, session: &DanmuSession, command: &str) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::UnhandledCommand,
            serde_json::json!({ "command": command }),
        )
    }

    pub(crate) fn assistant_stopped(
        &self,
        session: &DanmuSession,
        native_session: &str,
        reason: &str,
    ) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::AssistantStopped,
            serde_json::json!({ "native_session": native_session, "reason": reason }),
        )
    }
    pub(crate) fn assistant_routing(&self, session: &DanmuSession, snapshot: &Value) -> Result<()> {
        self.append(&session.id, JournalKind::AssistantRouting, snapshot)
    }

    pub fn snapshot(&self, session: &DanmuSession) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::SessionSnapshot,
            serde_json::to_value(session)?,
        )
    }

    pub fn end(&self, session: &DanmuSession) -> Result<()> {
        let result = self.append(
            &session.id,
            JournalKind::SessionEnded,
            serde_json::to_value(session)?,
        );
        let exports = self.write_exports(session);
        result.and(exports)
    }

    pub fn interrupt(&self, session: &DanmuSession) -> Result<()> {
        self.append(
            &session.id,
            JournalKind::SessionInterrupted,
            serde_json::to_value(session)?,
        )
    }

    pub fn recent_room_history(
        &self,
        room_id: &str,
        event_limit: usize,
    ) -> Result<Vec<DanmuEvent>> {
        let mut latest: Option<DanmuSession> = None;
        for id in self.session_ids()? {
            if let Some(mut session) = self.latest_session(&id)? {
                session
                    .recent_events
                    .retain(|event| event.kind.activity_lifetime().is_none());
                if session.room_id != room_id || session.recent_events.is_empty() {
                    continue;
                }
                let is_newer = latest
                    .as_ref()
                    .map(|current| session.started_at >= current.started_at)
                    .unwrap_or(true);
                if is_newer {
                    latest = Some(session);
                }
            }
        }
        let mut events = latest
            .map(|session| session.recent_events)
            .unwrap_or_default();
        events.sort_by_key(|event| std::cmp::Reverse(event.timestamp));
        events.truncate(event_limit);
        Ok(events)
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
        let (snapshot, summary) = serialize_exports(session)?;
        std::fs::write(directory.join("snapshot.json"), snapshot)?;
        std::fs::write(directory.join("summary.md"), summary)?;
        if let Some(mirror) = self
            .mirror
            .lock()
            .map_err(|_| anyhow::anyhow!("归档副本锁损坏"))?
            .as_mut()
        {
            mirror.sync_exports(&directory)?;
        }
        Ok(())
    }

    pub fn reply_record<T: Serialize>(&self, session_id: &str, record: &T) -> Result<()> {
        self.append(session_id, JournalKind::AutoReply, record)
    }

    pub fn reply_records<T: serde::de::DeserializeOwned>(
        &self,
        session_id: &str,
    ) -> Result<Vec<T>> {
        let path = self.session_directory(session_id).join("journal.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        read_records_strict(&path)?
            .into_iter()
            .filter(|record| record.kind == JournalKind::AutoReply)
            .map(|record| serde_json::from_value(record.payload).map_err(Into::into))
            .collect()
    }

    pub fn reply_records_for_stream<T: serde::de::DeserializeOwned>(
        &self,
        stream: &str,
    ) -> Result<Vec<T>> {
        let prefix = format!("{stream}:");
        let mut replies = Vec::new();
        for session in self.session_ids()? {
            for record in
                read_records_strict(&self.session_directory(&session).join("journal.jsonl"))?
            {
                if record.kind == JournalKind::AutoReply
                    && record
                        .payload
                        .get("key")
                        .and_then(Value::as_str)
                        .is_some_and(|key| key.starts_with(&prefix))
                {
                    replies.push(serde_json::from_value(record.payload)?);
                }
            }
        }
        Ok(replies)
    }

    fn append<T: Serialize>(&self, session_id: &str, kind: JournalKind, payload: T) -> Result<()> {
        ensure!(
            Path::new(session_id).components().count() == 1
                && session_id != "."
                && session_id != "..",
            "无效场次标识"
        );
        let mut mirror = self
            .mirror
            .lock()
            .map_err(|_| anyhow::anyhow!("归档副本锁损坏"))?;
        let directory = self.session_directory(session_id);
        check_path(&directory)?;
        std::fs::create_dir_all(&directory).context("创建会话归档目录失败")?;
        let path = directory.join("journal.jsonl");
        check_path(&path)?;
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        file.lock_exclusive()?;
        let length = file.metadata()?.len();
        let before = mirror
            .as_ref()
            .map(|_| FileStamp::read(&path))
            .transpose()?;
        if length > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut tail = [0];
            file.read_exact(&mut tail)?;
            ensure!(tail == *b"\n", "原始归档末条未完成；保留原件，禁止继续拼接");
        }
        let sequence = last_sequence(&mut file)?
            .checked_add(1)
            .context("归档序列号已溢出")?;
        let record = JournalRecord {
            sequence,
            timestamp: Utc::now(),
            kind,
            payload: serde_json::to_value(payload)?,
        };
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        if let Some(mirror) = mirror.as_mut() {
            mirror.sync_journal(&directory, &mut file, before.as_ref())?;
        }
        FileExt::unlock(&file)?;
        Ok(())
    }

    fn latest_session(&self, id: &str) -> Result<Option<DanmuSession>> {
        let path = self.session_directory(id).join("journal.jsonl");
        if let Some(session) = read_latest_session_state(&path)? {
            return Ok(Some(session));
        }
        replay_legacy_session(&path, id)
    }

    fn session_ids(&self) -> Result<Vec<String>> {
        check_path(&self.root)?;
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry?;
            ensure!(
                !entry.file_type()?.is_symlink(),
                "归档目录含链接；保留原件，停止同步"
            );
            if entry.file_type()?.is_dir() {
                let journal = entry.path().join("journal.jsonl");
                check_path(&journal)?;
                if journal.try_exists()? {
                    ensure!(journal.is_file(), "归档不是普通文件");
                    ids.push(
                        entry
                            .file_name()
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("场次名称不是UTF-8"))?,
                    );
                }
            }
        }
        Ok(ids)
    }

    fn session_directory(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
}

/// Reject links at every caller-controlled component (including dangling links).
pub(crate) fn check_path(path: &Path) -> Result<()> {
    for component in path.ancestors() {
        match std::fs::symlink_metadata(component) {
            Ok(metadata) => {
                // macOS exposes its root-owned /var and /tmp through system aliases.
                #[cfg(unix)]
                if metadata.file_type().is_symlink()
                    && component != path
                    && component.parent() == Some(Path::new("/"))
                {
                    use std::os::unix::fs::MetadataExt;
                    if metadata.uid() == 0 {
                        continue;
                    }
                }
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "维护数据路径含符号链接：{}；原件保留",
                    component.display()
                );
                ensure!(
                    metadata.is_file() || metadata.is_dir(),
                    "维护数据含特殊文件：{}",
                    component.display()
                );
                #[cfg(unix)]
                if metadata.is_file() {
                    use std::os::unix::fs::MetadataExt;
                    ensure!(
                        metadata.nlink() == 1,
                        "维护数据不能是硬链接：{}",
                        component.display()
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl FileStamp {
    fn read(path: &Path) -> Result<Self> {
        check_path(path)?;
        let metadata = std::fs::metadata(path)?;
        ensure!(metadata.is_file(), "维护副本不是普通文件");
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        })
    }
}

#[derive(Debug)]
struct JournalProgress {
    verified: u64,
    room: String,
    source: FileStamp,
}
#[derive(Deserialize)]
struct JournalMirrorRecord<'a> {
    kind: &'a str,
    #[serde(borrow)]
    payload: JournalMirrorPayload<'a>,
}

#[derive(Deserialize)]
struct JournalMirrorPayload<'a> {
    #[serde(rename = "roomId")]
    room_id: Option<&'a str>,
    #[serde(rename = "roomID")]
    legacy_room_id: Option<&'a str>,
}

#[derive(Debug)]
struct JournalMirror {
    root: PathBuf,
    room: Option<String>,
    files: BTreeMap<PathBuf, FileStamp>,
    journals: BTreeMap<PathBuf, JournalProgress>,
}
impl JournalMirror {
    fn target(&self, directory: &Path) -> Result<PathBuf> {
        let target = self
            .root
            .join(directory.file_name().context("缺少场次标识")?);
        check_path(&target)?;
        Ok(target)
    }

    fn sync_journal(
        &mut self,
        directory: &Path,
        source: &mut File,
        before_append: Option<&FileStamp>,
    ) -> Result<()> {
        let target_dir = self.target(directory)?;
        let target = target_dir.join("journal.jsonl");
        check_path(&target)?;
        let previous = self.files.get(&target);
        if let Some(stamp) = previous {
            ensure!(
                *stamp == FileStamp::read(&target)?,
                "归档副本被修改：{}；两边原件保留",
                target.display()
            );
        }
        let progress = self.journals.get(&target);
        let offset = progress.map_or(0, |progress| progress.verified);
        let verify_prefix = progress.is_none_or(|progress| before_append != Some(&progress.source));
        // The private journal is append-only. Snapshot its current extent and never let a
        // concurrent writer make this maintenance pass chase a growing file.
        let length = source.metadata()?.len();
        ensure!(
            length >= previous.map_or(0, |stamp| stamp.len),
            "原始归档已截断；两边保留"
        );
        source.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(Read::by_ref(source).take(length - offset));
        let mut room = progress.map(|progress| progress.room.clone());
        let mut verified = offset;
        let mut line = Vec::new();
        loop {
            line.clear();
            let count = (&mut reader)
                .take(8 * 1024 * 1024 + 1)
                .read_until(b'\n', &mut line)?;
            if count == 0 {
                break;
            }
            ensure!(count <= 8 * 1024 * 1024, "归档单条过大；未覆盖副本");
            if line.last() != Some(&b'\n') {
                break;
            }
            let record: JournalMirrorRecord<'_> =
                serde_json::from_slice(&line).context("原始归档损坏；未覆盖副本")?;
            verified += count as u64;
            if record.kind == "sessionStarted" {
                let id = record
                    .payload
                    .room_id
                    .or(record.payload.legacy_room_id)
                    .context("原始归档缺少房间")?;
                ensure!(
                    room.as_deref().is_none_or(|room| room == id),
                    "同一原始归档混入其他房间；未覆盖副本"
                );
                room = Some(id.into());
            }
        }
        ensure!(room.is_some(), "原始归档缺少场次开始记录；未覆盖副本");
        ensure!(
            source.metadata()?.len() >= length,
            "原始归档在同步期间被截断；两边保留"
        );
        if self
            .room
            .as_ref()
            .is_some_and(|expected| room.as_ref() != Some(expected))
        {
            return Ok(());
        }
        crate::bridge::wire::private_root(&target_dir)?;
        let mut options = OpenOptions::new();
        options.read(true).append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut destination = options.open(&target)?;
        destination.lock_exclusive()?;
        let existing = destination.metadata()?.len();
        ensure!(existing <= verified, "归档副本比原件完整前缀更长；两边保留");
        if let Some(stamp) = previous {
            ensure!(
                *stamp == FileStamp::read(&target)?,
                "归档副本在同步期间发生变化"
            );
        }
        if verify_prefix {
            source.seek(SeekFrom::Start(0))?;
            compare_prefix(source, &mut destination, existing)?;
        }
        source.seek(SeekFrom::Start(existing))?;
        std::io::copy(&mut source.take(verified - existing), &mut destination)?;
        destination.sync_data()?;
        self.files.insert(target.clone(), FileStamp::read(&target)?);
        self.journals.insert(
            target,
            JournalProgress {
                verified,
                room: room.context("缺少场次房间")?,
                source: FileStamp::read(&directory.join("journal.jsonl"))?,
            },
        );
        Ok(())
    }

    fn sync_exports(&mut self, directory: &Path) -> Result<()> {
        let target_dir = self.target(directory)?;
        if !self.files.contains_key(&target_dir.join("journal.jsonl")) {
            return Ok(());
        }
        for name in ["snapshot.json", "summary.md"] {
            let source = directory.join(name);
            let target = target_dir.join(name);
            check_path(&source)?;
            check_path(&target)?;
            if !source.try_exists()? {
                continue;
            }
            let bytes = std::fs::read(&source)?;
            if name == "snapshot.json" {
                serde_json::from_slice::<Value>(&bytes).context("原始快照损坏；两边保留")?;
            }
            if target.try_exists()? {
                if std::fs::read(&target)? == bytes {
                    self.files.insert(target.clone(), FileStamp::read(&target)?);
                    continue;
                }
                ensure!(
                    self.files.get(&target).is_some_and(
                        |stamp| FileStamp::read(&target).is_ok_and(|current| *stamp == current)
                    ),
                    "归档导出副本冲突：{}；两边保留",
                    target.display()
                );
                crate::storage::write_private_atomic(&target, &bytes)?;
            } else {
                let mut temporary = tempfile::NamedTempFile::new_in(&target_dir)?;
                temporary.write_all(&bytes)?;
                temporary.as_file().sync_all()?;
                temporary.persist_noclobber(&target)?;
            }
            self.files.insert(target.clone(), FileStamp::read(&target)?);
        }
        Ok(())
    }
}

fn compare_prefix(source: &mut File, target: &mut File, mut length: u64) -> Result<()> {
    let mut left = [0_u8; 16 * 1024];
    let mut right = [0_u8; 16 * 1024];
    while length > 0 {
        let count = length.min(left.len() as u64) as usize;
        source.read_exact(&mut left[..count])?;
        target.read_exact(&mut right[..count])?;
        ensure!(
            left[..count] == right[..count],
            "归档副本正文冲突；两边原件保留，未覆盖"
        );
        length -= count as u64;
    }
    Ok(())
}

fn last_sequence(file: &mut File) -> Result<u64> {
    #[derive(Deserialize)]
    struct Sequence {
        sequence: Option<u64>,
    }
    let last = read_reverse_records(file, |file, start, end| {
        if start >= end {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(start))?;
        let record: Sequence = serde_json::from_reader(Read::by_ref(file).take(end - start))
            .context("原始归档末条损坏；保留原件，禁止继续追加")?;
        Ok(Some(record.sequence))
    })?;
    match last {
        None => Ok(0),
        Some(Some(sequence)) => Ok(sequence),
        Some(None) => {
            // Pre-sequence journals need one count; the appended record establishes the tail cursor.
            file.seek(SeekFrom::Start(0))?;
            let mut block = [0_u8; REVERSE_READ_BLOCK_BYTES];
            let mut lines = 0;
            loop {
                let count = file.read(&mut block)?;
                if count == 0 {
                    return Ok(lines);
                }
                lines += block[..count].iter().filter(|&&byte| byte == b'\n').count() as u64;
            }
        }
    }
}

fn read_records_strict(path: &Path) -> Result<Vec<JournalRecord>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    BufReader::new(file)
        .lines()
        .map(|line| serde_json::from_str(&line?).context("自动回复日志损坏；不能安全恢复去重状态"))
        .collect()
}

const REVERSE_READ_BLOCK_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
struct JournalRecordHeader {
    #[serde(rename = "sequence")]
    _sequence: u64,
    #[serde(rename = "timestamp")]
    _timestamp: DateTime<Utc>,
    kind: JournalKind,
}

#[derive(Deserialize)]
struct JournalSessionPayload {
    payload: DanmuSession,
}

fn read_latest_session_state(path: &Path) -> Result<Option<DanmuSession>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    FileExt::lock_shared(&file)?;
    read_reverse_records(&mut file, read_session_state_line)
}

fn read_reverse_records<T>(
    file: &mut File,
    mut visit: impl FnMut(&mut File, u64, u64) -> Result<Option<T>>,
) -> Result<Option<T>> {
    // Scan delimiters from the tail and deserialize only the bounded records requested.
    let mut line_end = file.metadata()?.len();
    let mut cursor = line_end;
    let mut block = [0_u8; REVERSE_READ_BLOCK_BYTES];
    while cursor > 0 {
        let block_start = cursor.saturating_sub(REVERSE_READ_BLOCK_BYTES as u64);
        let count = usize::try_from(cursor - block_start)?;
        file.seek(SeekFrom::Start(block_start))?;
        file.read_exact(&mut block[..count])?;

        for index in (0..count).rev() {
            if block[index] != b'\n' {
                continue;
            }
            let line_start = block_start + index as u64 + 1;
            if let Some(value) = visit(file, line_start, line_end)? {
                return Ok(Some(value));
            }
            line_end = line_start - 1;
        }
        cursor = block_start;
    }
    visit(file, 0, line_end)
}

fn read_session_state_line(file: &mut File, start: u64, end: u64) -> Result<Option<DanmuSession>> {
    if start >= end {
        return Ok(None);
    }
    let length = end - start;
    file.seek(SeekFrom::Start(start))?;
    let kind =
        match serde_json::from_reader::<_, JournalRecordHeader>(Read::by_ref(file).take(length)) {
            Ok(record) => record.kind,
            Err(_) => return Ok(None),
        };
    if !matches!(
        kind,
        JournalKind::SessionStarted
            | JournalKind::SessionSnapshot
            | JournalKind::SessionEnded
            | JournalKind::SessionInterrupted
    ) {
        return Ok(None);
    }

    file.seek(SeekFrom::Start(start))?;
    let Ok(mut record) =
        serde_json::from_reader::<_, JournalSessionPayload>(Read::by_ref(file).take(length))
    else {
        return Ok(None);
    };
    record.payload.restore_runtime_state();
    Ok(Some(record.payload))
}

#[cfg(test)]
fn read_records(path: &Path) -> Result<Vec<JournalRecord>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect())
}

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
    fn malformed_journal_does_not_prevent_other_sessions_from_being_indexed() {
        let root = tempfile::tempdir().unwrap();
        let journal = SessionJournal::new(root.path().join("sessions"));
        let bad = DanmuSession::new("123");
        journal.start(&bad).unwrap();
        let bad_path = journal.session_directory(&bad.id).join("journal.jsonl");
        let original = std::fs::read(&bad_path).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&bad_path)
            .unwrap()
            .write_all(b"broken JSON\n")
            .unwrap();
        let good = DanmuSession::new("123");
        journal.start(&good).unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "完整归档继续检索");
        journal.event(&good, &event).unwrap();
        let history = crate::history::History::open(root.path(), "123").unwrap();
        assert!(journal.index_history(&history).is_err());
        assert_eq!(
            history
                .search(["完整归档"].into_iter(), "", &[], 4096)
                .unwrap()[0]["id"],
            event.id
        );
        std::fs::write(&bad_path, original).unwrap();
        journal.index_history(&history).unwrap();
        assert_eq!(
            history
                .search(["完整归档"].into_iter(), "", &[], 4096)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn active_session_events_are_available_as_room_history() {
        let temp = tempfile::tempdir().unwrap();
        let journal = SessionJournal::new(temp.path().into());
        let mut session = DanmuSession::new("42");
        journal.start(&session).unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "怎么做？");
        session.ingest(event.clone());
        journal.event(&session, &event).unwrap();

        let history = journal.recent_room_history("42", 240).unwrap();
        assert_eq!(history, vec![event]);
    }
    #[test]
    fn tail_scan_ignores_non_state_corruption_and_stale_export() {
        let temp = tempfile::tempdir().unwrap();
        let journal = SessionJournal::new(temp.path().into());
        let session = DanmuSession::new("journal-truth");
        journal.start(&session).unwrap();
        journal
            .append(
                &session.id,
                JournalKind::UnhandledCommand,
                serde_json::json!({ "padding": "x".repeat(REVERSE_READ_BLOCK_BYTES + 1) }),
            )
            .unwrap();

        let path = journal.session_directory(&session.id).join("journal.jsonl");
        std::fs::write(
            journal.session_directory(&session.id).join("snapshot.json"),
            serde_json::to_vec(&DanmuSession::new("stale-export")).unwrap(),
        )
        .unwrap();
        let mut tail = OpenOptions::new().append(true).open(&path).unwrap();
        serde_json::to_writer(
            &mut tail,
            &JournalRecord {
                sequence: 3,
                timestamp: Utc::now(),
                kind: JournalKind::SessionSnapshot,
                payload: serde_json::json!({}),
            },
        )
        .unwrap();
        tail.write_all(
            br#"
{"kind":"sessionInterrupted""#,
        )
        .unwrap();

        let restored = journal.latest_session(&session.id).unwrap().unwrap();
        assert_eq!(restored.id, session.id);
        assert_eq!(restored.room_id, "journal-truth");
    }

    #[test]
    fn room_history_uses_latest_nonempty_session_regardless_of_end_state() {
        let temp = tempfile::tempdir().unwrap();
        let journal = SessionJournal::new(temp.path().into());
        let now = Utc::now();

        let mut ended = DanmuSession::with_options("42", 240, now - chrono::Duration::minutes(3));
        journal.start(&ended).unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "正常关闭前的弹幕");
        ended.ingest(event.clone());
        journal.event(&ended, &event).unwrap();
        ended.end(
            now - chrono::Duration::minutes(2),
            DanmuSessionEndReason::Completed,
        );
        journal.end(&ended).unwrap();
        let source = journal.session_directory(&ended.id).join("journal.jsonl");
        let original = std::fs::read(&source).unwrap();

        let mut empty = DanmuSession::with_options("42", 240, now - chrono::Duration::minutes(1));
        empty.ingest(DanmuEvent::new(DanmuEventKind::Enter, "仅有临时活动"));
        journal.start(&empty).unwrap();
        let mut other = DanmuSession::with_options("99", 240, now);
        let other_event = DanmuEvent::new(DanmuEventKind::Danmu, "其他房间更新");
        other.ingest(other_event.clone());
        journal.start(&other).unwrap();

        assert_eq!(journal.recent_room_history("42", 240).unwrap(), vec![event]);
        assert_eq!(
            journal.recent_room_history("99", 240).unwrap(),
            vec![other_event]
        );
        assert_eq!(std::fs::read(source).unwrap(), original);
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
    fn stopped_assistant_diagnostic_does_not_end_or_hide_the_live_session() {
        let temp = tempfile::tempdir().unwrap();
        let journal = SessionJournal::new(temp.path().into());
        let mut session = DanmuSession::new("42");
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "still live");
        session.ingest(event);
        journal.start(&session).unwrap();
        journal
            .assistant_stopped(&session, "native-42", "ACP disconnected")
            .unwrap();
        let reopened = SessionJournal::new(temp.path().into());
        let restored = reopened.recent_room_history("42", 240).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].content, "still live");
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

    #[test]
    fn workspace_archives_keep_complete_sessions_and_follow_append_after_reopen() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        let workspace = root.path().join("workspace");
        let journal = SessionJournal::new(private.clone());
        let mut first = DanmuSession::new("123");
        journal.start(&first).unwrap();
        for index in 0..620 {
            let mut event = DanmuEvent::new(DanmuEventKind::Danmu, format!("历史正文-{index}"));
            event.id = format!("old-{index}");
            first.ingest(event.clone());
            journal
                .append(&first.id, JournalKind::EventReceived, event)
                .unwrap();
        }
        journal.snapshot(&first).unwrap();
        journal.interrupt(&first).unwrap();
        let foreign = DanmuSession::new("456");
        journal.start(&foreign).unwrap();
        journal.bind_workspace(&workspace, "123").unwrap();
        let copy = workspace.join(".danmu/sessions").join(&first.id);
        assert_eq!(
            std::fs::read(copy.join("journal.jsonl")).unwrap(),
            std::fs::read(private.join(&first.id).join("journal.jsonl")).unwrap()
        );
        assert!(!workspace.join(".danmu/sessions").join(&foreign.id).exists());
        first.end(Utc::now(), DanmuSessionEndReason::Completed);
        journal.end(&first).unwrap();
        assert_eq!(
            std::fs::read(copy.join("snapshot.json")).unwrap(),
            std::fs::read(private.join(&first.id).join("snapshot.json")).unwrap()
        );
        let records = read_records_strict(&copy.join("journal.jsonl")).unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|row| row.kind == JournalKind::EventReceived)
                .count(),
            620
        );
        assert!(
            records
                .iter()
                .any(|row| row.kind == JournalKind::SessionInterrupted)
        );
        assert_eq!(records.last().unwrap().kind, JournalKind::SessionEnded);
        drop(journal);
        let reopened = SessionJournal::new(private.clone());
        reopened.bind_workspace(&workspace, "123").unwrap();
        let mut second = DanmuSession::new("123");
        reopened.start(&second).unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "跨场追加正文");
        second.ingest(event.clone());
        reopened.event(&second, &event).unwrap();
        let second_copy = workspace
            .join(".danmu/sessions")
            .join(&second.id)
            .join("journal.jsonl");
        assert_eq!(
            std::fs::read(second_copy).unwrap(),
            std::fs::read(private.join(&second.id).join("journal.jsonl")).unwrap()
        );
        let index = crate::history::History::open(&workspace.join(".danmu"), "123").unwrap();
        SessionJournal::new(workspace.join(".danmu/sessions"))
            .index_history(&index)
            .unwrap();
        assert!(
            index
                .search(["历史正文-0"].into_iter(), "", &[], 200_000)
                .unwrap()
                .iter()
                .any(|row| row["id"] == "old-0")
        );
        assert!(
            index
                .search(["跨场追加正文"].into_iter(), "", &[], 4096)
                .unwrap()
                .iter()
                .any(|row| row["id"] == event.id)
        );
    }

    #[test]
    fn workspace_conflicts_and_damaged_sources_preserve_both_copies() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        let workspace = root.path().join("workspace");
        let journal = SessionJournal::new(private.clone());
        let mut session = DanmuSession::new("123");
        journal.start(&session).unwrap();
        journal.bind_workspace(&workspace, "123").unwrap();
        let target = workspace
            .join(".danmu/sessions")
            .join(&session.id)
            .join("journal.jsonl");
        let original = std::fs::read(&target).unwrap();
        let conflicting = String::from_utf8(original.clone())
            .unwrap()
            .replace("123", "456");
        std::fs::write(&target, &conflicting).unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "副本冲突但私有原件仍保存");
        session.ingest(event.clone());
        assert!(journal.event(&session, &event).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), conflicting);
        assert!(
            read_records_strict(&private.join(&session.id).join("journal.jsonl"))
                .unwrap()
                .iter()
                .any(|row| row.kind == JournalKind::EventReceived && row.payload["id"] == event.id)
        );
        assert!(
            journal
                .latest_session(&session.id)
                .unwrap()
                .unwrap()
                .recent_events
                .iter()
                .any(|saved| saved.id == event.id)
        );
        assert!(
            SessionJournal::new(private.clone())
                .bind_workspace(&workspace, "123")
                .is_err()
        );
        let source = private.join(&session.id).join("journal.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&source)
            .unwrap()
            .write_all(b"{broken}\n")
            .unwrap();
        assert!(
            SessionJournal::new(private)
                .bind_workspace(&root.path().join("fresh"), "123")
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), conflicting);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_symlink_and_unfinished_source_fail_without_touching_targets() {
        let root = tempfile::tempdir().unwrap();
        let journal = SessionJournal::new(root.path().join("private"));
        let session = DanmuSession::new("123");
        journal.start(&session).unwrap();
        let workspace = root.path().join("workspace");
        let managed = crate::workspace::managed_root(&workspace).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, managed.join("sessions")).unwrap();
        assert!(journal.bind_workspace(&workspace, "123").is_err());
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        let source = journal.session_directory(&session.id).join("journal.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&source)
            .unwrap()
            .write_all(b"{partial")
            .unwrap();
        let bytes = std::fs::read(&source).unwrap();
        assert!(journal.snapshot(&session).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
    }

    #[test]
    fn workspace_copy_ignores_a_partial_tail_and_rejects_rewrites() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        let journal = SessionJournal::new(private.clone());
        let session = DanmuSession::new("123");
        journal.start(&session).unwrap();
        let source = journal.session_directory(&session.id).join("journal.jsonl");
        let complete_prefix = std::fs::read(&source).unwrap();
        let event = DanmuEvent::new(DanmuEventKind::Danmu, "分段写入的历史正文");
        let record = serde_json::to_vec(&JournalRecord {
            sequence: 2,
            timestamp: Utc::now(),
            kind: JournalKind::EventReceived,
            payload: serde_json::to_value(&event).unwrap(),
        })
        .unwrap();
        let midpoint = record.len() / 2;
        let mut file = OpenOptions::new().append(true).open(&source).unwrap();
        file.write_all(&record[..midpoint]).unwrap();
        file.sync_data().unwrap();

        let workspace = root.path().join("workspace");
        SessionJournal::new(private)
            .bind_workspace(&workspace, "123")
            .unwrap();
        let target = workspace
            .join(".danmu/sessions")
            .join(&session.id)
            .join("journal.jsonl");
        assert_eq!(std::fs::read(&target).unwrap(), complete_prefix);

        file.write_all(&record[midpoint..]).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_data().unwrap();
        journal.bind_workspace(&workspace, "123").unwrap();
        journal.snapshot(&session).unwrap();
        assert_eq!(
            std::fs::read(&target).unwrap(),
            std::fs::read(&source).unwrap()
        );
        assert!(
            read_records_strict(&target)
                .unwrap()
                .iter()
                .any(|row| row.payload["id"] == event.id)
        );
        let original_target = std::fs::read(&target).unwrap();
        let modified = std::fs::read_to_string(&source)
            .unwrap()
            .replace("123", "456");
        std::fs::write(&source, modified).unwrap();
        assert!(journal.snapshot(&session).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), original_target);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_copy_does_not_wait_for_an_advisory_source_lock() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        let journal = SessionJournal::new(private.clone());
        let session = DanmuSession::new("123");
        journal.start(&session).unwrap();
        let source = journal.session_directory(&session.id).join("journal.jsonl");
        let locked = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&source)
            .unwrap();
        locked.lock_exclusive().unwrap();

        let workspace = root.path().join("workspace");
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            sender
                .send(SessionJournal::new(private).bind_workspace(&workspace, "123"))
                .unwrap();
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("workspace copy waited for an advisory source lock");
        FileExt::unlock(&locked).unwrap();
        result.unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn workspace_copy_propagates_a_mandatory_source_lock_and_recovers() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        let journal = SessionJournal::new(private.clone());
        let session = DanmuSession::new("123");
        journal.start(&session).unwrap();
        let source = journal.session_directory(&session.id).join("journal.jsonl");
        let locked = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&source)
            .unwrap();
        locked.lock_exclusive().unwrap();

        let workspace = root.path().join("workspace");
        let error = SessionJournal::new(private.clone())
            .bind_workspace(&workspace, "123")
            .unwrap_err();
        assert!(error.downcast_ref::<std::io::Error>().is_some());

        FileExt::unlock(&locked).unwrap();
        SessionJournal::new(private)
            .bind_workspace(&workspace, "123")
            .unwrap();
        assert_eq!(
            std::fs::read(
                workspace
                    .join(".danmu/sessions")
                    .join(&session.id)
                    .join("journal.jsonl")
            )
            .unwrap(),
            std::fs::read(source).unwrap()
        );
    }
    #[test]
    fn same_archive_migration_through_a_path_alias_never_locks_itself() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let managed = crate::workspace::managed_root(&workspace).unwrap();
        crate::bridge::wire::private_root(&managed.join("sessions")).unwrap();
        let journal = SessionJournal::new(managed.join("sessions"));
        let session = DanmuSession::new("123");
        journal.start(&session).unwrap();
        let path = managed
            .join("sessions")
            .join(&session.id)
            .join("journal.jsonl");
        let original = std::fs::read(&path).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                journal.migrate_to(&workspace.join("."))?;
                journal.bind_workspace(&workspace, "123")?;
                journal.snapshot(&session)
            })();
            sender.send(result).unwrap();
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("同目录迁移不应死锁")
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(&original));
        assert_eq!(
            read_records_strict(&path).unwrap().last().unwrap().kind,
            JournalKind::SessionSnapshot
        );
    }
}
