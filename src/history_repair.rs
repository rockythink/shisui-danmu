use crate::{
    domain::DanmuSession,
    persistence::{JournalKind, JournalRecord, check_path},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_EXPORT_BYTES: u64 = 16 * 1024 * 1024;
const EXPORTS: [&str; 2] = ["snapshot.json", "summary.md"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepairReport {
    pub repaired_files: usize,
    pub backup_dir: Option<PathBuf>,
}

struct Candidate {
    session_id: String,
    source: PathBuf,
    target: PathBuf,
    source_bytes: Vec<u8>,
    target_bytes: Vec<u8>,
    source_journal_stamp: Option<JournalStamp>,
    target_journal_stamp: Option<JournalStamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JournalStamp {
    length: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl JournalStamp {
    fn read(file: &File) -> Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            length: metadata.len(),
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

struct JournalFacts {
    latest: DanmuSession,
    target_snapshot_seen: bool,
    target_summary_seen: bool,
}

/// Repairs only stale, program-derived workspace exports. Journals remain the recovery truth.
/// This maintenance path never restores, grants, or changes permission to send messages.
pub(crate) fn repair_exports(private_root: &Path, workspace: &Path) -> Result<RepairReport> {
    check_path(private_root)?;
    check_path(workspace)?;
    let managed = crate::workspace::managed_root(workspace)?;
    check_path(&managed)?;
    let sessions = managed.join("sessions");
    check_path(&sessions)?;

    let entries = match std::fs::read_dir(&sessions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RepairReport {
                repaired_files: 0,
                backup_dir: None,
            });
        }
        Err(error) => return Err(error.into()),
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        ensure!(
            !entry.file_type()?.is_symlink(),
            "工作区会话目录含符号链接；两边保留"
        );
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("场次名称不是 UTF-8"))?;
        ensure!(valid_id(&id), "无效场次标识");
        let source_dir = private_root.join(&id);
        let target_dir = entry.path();
        check_path(&source_dir)?;
        check_path(&target_dir)?;
        if !source_dir.try_exists()? {
            continue;
        }

        let mut session_candidates = Vec::new();
        for name in EXPORTS {
            let source = source_dir.join(name);
            let target = target_dir.join(name);
            check_path(&source)?;
            check_path(&target)?;
            // Missing exports are populated by the ordinary bind flow, not by historical repair.
            if !source.try_exists()? || !target.try_exists()? {
                continue;
            }
            let source_bytes = read_regular_limited(&source, MAX_EXPORT_BYTES)?;
            let target_bytes = read_regular_limited(&target, MAX_EXPORT_BYTES)?;
            if source_bytes != target_bytes {
                session_candidates.push(Candidate {
                    session_id: id.clone(),
                    source,
                    target,
                    source_bytes,
                    target_bytes,
                    source_journal_stamp: None,
                    target_journal_stamp: None,
                });
            }
        }
        if session_candidates.is_empty() {
            continue;
        }
        let (source_stamp, target_stamp) =
            validate_session(&source_dir, &target_dir, &session_candidates)?;
        for candidate in &mut session_candidates {
            candidate.source_journal_stamp = Some(source_stamp.clone());
            candidate.target_journal_stamp = Some(target_stamp.clone());
        }
        candidates.extend(session_candidates);
    }

    if candidates.is_empty() {
        return Ok(RepairReport {
            repaired_files: 0,
            backup_dir: None,
        });
    }

    let backup = managed.join(format!("history-repair-{}", uuid::Uuid::new_v4()));
    check_path(&backup)?;
    std::fs::create_dir(&backup).context("创建历史维修备份目录失败")?;
    #[cfg(unix)]
    sync_directory(&managed)?;

    for candidate in &candidates {
        replace_one(candidate, &backup).with_context(|| {
            format!(
                "历史维修未全部完成；已创建的备份保留在 {}",
                backup.display()
            )
        })?;
    }
    #[cfg(unix)]
    sync_directory(&backup)?;

    Ok(RepairReport {
        repaired_files: candidates.len(),
        backup_dir: Some(backup),
    })
}

fn validate_session(
    source_dir: &Path,
    target_dir: &Path,
    candidates: &[Candidate],
) -> Result<(JournalStamp, JournalStamp)> {
    let source_journal_path = source_dir.join("journal.jsonl");
    let target_journal_path = target_dir.join("journal.jsonl");
    check_path(&source_journal_path)?;
    check_path(&target_journal_path)?;
    let mut source_journal = open_regular(&source_journal_path)?;
    FileExt::lock_shared(&source_journal)?;
    let mut target_journal = open_regular(&target_journal_path)?;
    FileExt::lock_shared(&target_journal)?;

    let target_snapshot = candidates
        .iter()
        .find(|candidate| candidate.target.ends_with("snapshot.json"))
        .map(|candidate| {
            serde_json::from_slice::<DanmuSession>(&candidate.target_bytes)
                .context("工作区旧快照损坏；两边保留")
        })
        .transpose()?;
    let target_summary = candidates
        .iter()
        .find(|candidate| candidate.target.ends_with("summary.md"))
        .map(|candidate| candidate.target_bytes.as_slice());

    let source_facts = validate_journal(
        &mut source_journal,
        candidates[0].session_id.as_str(),
        target_snapshot.as_ref(),
        target_summary,
    )?;
    let target_facts = validate_journal(
        &mut target_journal,
        candidates[0].session_id.as_str(),
        target_snapshot.as_ref(),
        target_summary,
    )?;
    ensure!(
        source_facts.latest.room_id == target_facts.latest.room_id,
        "原件与工作区场次房间不一致；两边保留"
    );
    compare_journals(&mut source_journal, &mut target_journal)?;
    let source_stamp = JournalStamp::read(&source_journal)?;
    let target_stamp = JournalStamp::read(&target_journal)?;
    let (canonical_snapshot, canonical_summary) =
        crate::persistence::serialize_exports(&source_facts.latest)?;

    for candidate in candidates {
        if candidate.source.ends_with("snapshot.json") {
            let source_snapshot: DanmuSession = serde_json::from_slice(&candidate.source_bytes)
                .context("原始快照损坏；两边保留")?;
            ensure!(
                source_snapshot == source_facts.latest,
                "原始快照不是日志中的最近有效状态；两边保留"
            );
            // HashMap order and JSON whitespace can change across reads or versions.
            // Compare all JSON fields, not just the typed projection that ignores unknowns.
            ensure!(
                serde_json::from_slice::<serde_json::Value>(&candidate.source_bytes)?
                    == serde_json::from_slice::<serde_json::Value>(&canonical_snapshot)?,
                "原始快照含日志无法验证的数据；两边保留"
            );
            ensure!(
                source_snapshot.id == candidate.session_id
                    && source_snapshot.room_id == source_facts.latest.room_id,
                "原始快照场次或房间不一致；两边保留"
            );
            ensure!(
                target_facts.target_snapshot_seen,
                "工作区快照不是日志产生的旧导出；两边保留"
            );
        } else {
            ensure!(
                candidate.source_bytes == canonical_summary,
                "原始摘要不是日志最近状态的派生导出；两边保留"
            );
            ensure!(
                target_facts.target_summary_seen,
                "工作区摘要不是日志产生的旧导出；两边保留"
            );
        }
    }
    Ok((source_stamp, target_stamp))
}

fn validate_journal(
    file: &mut File,
    session_id: &str,
    target_snapshot: Option<&DanmuSession>,
    target_summary: Option<&[u8]>,
) -> Result<JournalFacts> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut expected_sequence = 1;
    let mut room: Option<String> = None;
    let mut latest = None;
    let mut target_snapshot_seen = target_snapshot.is_none();
    let mut target_summary_seen = target_summary.is_none();

    loop {
        line.clear();
        let count = Read::by_ref(&mut reader)
            .take(MAX_RECORD_BYTES + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        ensure!(count as u64 <= MAX_RECORD_BYTES, "归档单条过大；两边保留");
        ensure!(line.last() == Some(&b'\n'), "原始归档末条未完成；两边保留");
        let record: JournalRecord =
            serde_json::from_slice(&line).context("归档结构损坏；两边保留")?;
        ensure!(
            record.sequence == expected_sequence,
            "归档序号不连续；两边保留"
        );
        if expected_sequence == 1 {
            ensure!(
                record.kind == JournalKind::SessionStarted,
                "归档未以会话开始记录起始；两边保留"
            );
        }
        expected_sequence += 1;
        if matches!(
            record.kind,
            JournalKind::SessionStarted
                | JournalKind::SessionSnapshot
                | JournalKind::SessionEnded
                | JournalKind::SessionInterrupted
        ) {
            let session: DanmuSession =
                serde_json::from_value(record.payload).context("归档会话状态损坏；两边保留")?;
            ensure!(session.id == session_id, "归档场次标识不一致；两边保留");
            match &room {
                Some(room) => ensure!(*room == session.room_id, "归档房间不一致；两边保留"),
                None => room = Some(session.room_id.clone()),
            }
            if target_snapshot.is_some_and(|target| target == &session) {
                target_snapshot_seen = true;
            }
            let canonical_summary = crate::persistence::serialize_exports(&session)?.1;
            if target_summary.is_some_and(|target| target == canonical_summary) {
                target_summary_seen = true;
            }
            latest = Some(session);
        }
    }
    ensure!(expected_sequence > 1, "归档为空；两边保留");
    let latest = latest.context("归档缺少有效会话状态；两边保留")?;
    Ok(JournalFacts {
        latest,
        target_snapshot_seen,
        target_summary_seen,
    })
}

fn compare_journals(source: &mut File, target: &mut File) -> Result<()> {
    let source_len = source.metadata()?.len();
    let target_len = target.metadata()?.len();
    ensure!(target_len <= source_len, "工作区归档长于原件；两边保留");
    source.seek(SeekFrom::Start(0))?;
    target.seek(SeekFrom::Start(0))?;
    let mut remaining = target_len;
    let mut left = [0_u8; 32 * 1024];
    let mut right = [0_u8; 32 * 1024];
    while remaining > 0 {
        let count = remaining.min(left.len() as u64) as usize;
        source.read_exact(&mut left[..count])?;
        target.read_exact(&mut right[..count])?;
        ensure!(
            left[..count] == right[..count],
            "归档副本正文冲突；两边保留"
        );
        remaining -= count as u64;
    }
    Ok(())
}

fn replace_one(candidate: &Candidate, backup_root: &Path) -> Result<()> {
    let source_journal_path = candidate
        .source
        .parent()
        .context("原始导出缺少父目录")?
        .join("journal.jsonl");
    let target_journal_path = candidate
        .target
        .parent()
        .context("工作区导出缺少父目录")?
        .join("journal.jsonl");
    let source_journal = open_regular(&source_journal_path)?;
    FileExt::lock_shared(&source_journal)?;
    let target_journal = open_regular(&target_journal_path)?;
    FileExt::lock_shared(&target_journal)?;
    let source_stamp = candidate
        .source_journal_stamp
        .as_ref()
        .context("缺少原始归档校验指纹")?;
    let target_stamp = candidate
        .target_journal_stamp
        .as_ref()
        .context("缺少工作区归档校验指纹")?;
    ensure!(
        JournalStamp::read(&source_journal)? == *source_stamp
            && JournalStamp::read(&target_journal)? == *target_stamp,
        "归档在校验与替换之间发生变化；两边保留"
    );
    check_path(&candidate.source)?;
    check_path(&candidate.target)?;
    let mut source = open_regular(&candidate.source)?;
    FileExt::lock_shared(&source)?;
    let mut target = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&candidate.target)?;
    ensure_regular_file(&candidate.target, &target)?;
    FileExt::lock_exclusive(&target)?;
    let source_bytes = read_locked_limited(&mut source, MAX_EXPORT_BYTES)?;
    let target_bytes = read_locked_limited(&mut target, MAX_EXPORT_BYTES)?;
    ensure!(
        source_bytes == candidate.source_bytes && target_bytes == candidate.target_bytes,
        "导出文件在校验与替换之间发生变化；两边保留"
    );

    let session_backup = backup_root.join(&candidate.session_id);
    check_path(&session_backup)?;
    std::fs::create_dir_all(&session_backup)?;
    let backup_path =
        session_backup.join(candidate.target.file_name().context("导出文件缺少名称")?);
    write_new_atomic(&backup_path, &target_bytes)?;
    #[cfg(unix)]
    sync_directory(&session_backup)?;
    crate::storage::write_private_atomic(&candidate.target, &source_bytes)?;
    #[cfg(unix)]
    sync_directory(candidate.target.parent().context("导出文件缺少父目录")?)?;
    ensure!(
        JournalStamp::read(&source_journal)? == *source_stamp
            && JournalStamp::read(&target_journal)? == *target_stamp,
        "归档在维修期间发生变化；备份已保留"
    );
    Ok(())
}

fn read_regular_limited(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut file = open_regular(path)?;
    read_locked_limited(&mut file, limit)
}

fn read_locked_limited(file: &mut File, limit: u64) -> Result<Vec<u8>> {
    let length = file.metadata()?.len();
    ensure!(length <= limit, "导出文件过大；两边保留");
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(length as usize);
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn open_regular(path: &Path) -> Result<File> {
    check_path(path)?;
    let file = File::open(path)?;
    ensure_regular_file(path, &file)?;
    Ok(file)
}

fn ensure_regular_file(path: &Path, file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "维护数据不是普通文件：{}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.nlink() == 1,
            "维护数据不能是硬链接：{}",
            path.display()
        );
    }
    Ok(())
}

fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    check_path(path)?;
    let parent = path.parent().context("备份文件缺少父目录")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

// Unix additionally syncs directory metadata. Windows keeps the file sync before
// atomic publication above; it does not expose an equivalent directory fsync.
#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn valid_id(id: &str) -> bool {
    Path::new(id).components().count() == 1 && id != "." && id != ".."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{DanmuEvent, DanmuEventKind, DanmuSessionEndReason},
        persistence::SessionJournal,
    };
    use chrono::{TimeZone, Utc};

    struct Fixture {
        _temp: tempfile::TempDir,
        private: PathBuf,
        workspace: PathBuf,
        id: String,
        old_snapshot: Vec<u8>,
        old_summary: Vec<u8>,
    }

    impl Fixture {
        fn stale() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let private = temp.path().join("private");
            let workspace = temp.path().join("workspace");
            std::fs::create_dir_all(&private).unwrap();
            std::fs::create_dir_all(workspace.join(".danmu/sessions")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    workspace.join(".danmu"),
                    std::fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
            let journal = SessionJournal::new(private.clone());
            let mut session = DanmuSession::with_options(
                "7788",
                20,
                Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
            );
            journal.start(&session).unwrap();
            let old_snapshot = serde_json::to_vec_pretty(&session).unwrap();
            let old_summary = crate::persistence::serialize_exports(&session).unwrap().1;
            session.ingest(DanmuEvent::new(DanmuEventKind::Danmu, "new event"));
            session.ingest(DanmuEvent::new(DanmuEventKind::Like, "like"));
            session.ingest(DanmuEvent::new(DanmuEventKind::Follow, "follow"));
            journal.snapshot(&session).unwrap();
            session.end(
                Utc.with_ymd_and_hms(2026, 9, 1, 11, 0, 0).unwrap(),
                DanmuSessionEndReason::Completed,
            );
            journal.end(&session).unwrap();
            let id = session.id.clone();
            let source = private.join(&id);
            let target = workspace.join(".danmu/sessions").join(&id);
            std::fs::create_dir_all(&target).unwrap();
            std::fs::copy(source.join("journal.jsonl"), target.join("journal.jsonl")).unwrap();
            std::fs::write(target.join("snapshot.json"), &old_snapshot).unwrap();
            std::fs::write(target.join("summary.md"), &old_summary).unwrap();
            Self {
                _temp: temp,
                private,
                workspace,
                id,
                old_snapshot,
                old_summary,
            }
        }

        fn target(&self) -> PathBuf {
            self.workspace.join(".danmu/sessions").join(&self.id)
        }
    }

    #[test]
    fn repairs_stale_exports_and_preserves_original_backup() {
        let fixture = Fixture::stale();
        let report = repair_exports(&fixture.private, &fixture.workspace).unwrap();
        assert_eq!(report.repaired_files, 2);
        let backup = report.backup_dir.unwrap().join(&fixture.id);
        assert_eq!(
            std::fs::read(backup.join("snapshot.json")).unwrap(),
            fixture.old_snapshot
        );
        assert_eq!(
            std::fs::read(backup.join("summary.md")).unwrap(),
            fixture.old_summary
        );
        for name in EXPORTS {
            assert_eq!(
                std::fs::read(fixture.target().join(name)).unwrap(),
                std::fs::read(fixture.private.join(&fixture.id).join(name)).unwrap()
            );
        }
    }

    #[test]
    fn repairs_equivalent_json_without_requiring_current_serializer_layout() {
        let fixture = Fixture::stale();
        let source = fixture.private.join(&fixture.id).join("snapshot.json");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
        // JSON object order and whitespace are not persisted data contracts.
        let bytes = serde_json::to_vec(&value).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        let report = repair_exports(&fixture.private, &fixture.workspace).unwrap();
        assert_eq!(
            std::fs::read(fixture.target().join("snapshot.json")).unwrap(),
            bytes
        );
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert_eq!(
            std::fs::read(
                report
                    .backup_dir
                    .unwrap()
                    .join(&fixture.id)
                    .join("snapshot.json")
            )
            .unwrap(),
            fixture.old_snapshot
        );
        assert_eq!(
            repair_exports(&fixture.private, &fixture.workspace)
                .unwrap()
                .repaired_files,
            0
        );
    }

    #[test]
    fn source_snapshot_unjournaled_fields_are_not_discarded() {
        let fixture = Fixture::stale();
        let source = fixture.private.join(&fixture.id).join("snapshot.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
        value["futureMetadata"] = serde_json::json!({"note": "preserve me"});
        let bytes = serde_json::to_vec(&value).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        assert!(repair_exports(&fixture.private, &fixture.workspace).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert_eq!(
            std::fs::read(fixture.target().join("snapshot.json")).unwrap(),
            fixture.old_snapshot
        );
    }

    #[test]
    fn conflicting_journal_changes_nothing() {
        let fixture = Fixture::stale();
        let journal = fixture.target().join("journal.jsonl");
        let mut bytes = std::fs::read(&journal).unwrap();
        let position = bytes.iter().position(|byte| *byte == b'7').unwrap();
        bytes[position] = b'9';
        std::fs::write(&journal, bytes).unwrap();
        let before = std::fs::read(fixture.target().join("snapshot.json")).unwrap();
        assert!(repair_exports(&fixture.private, &fixture.workspace).is_err());
        assert_eq!(
            std::fs::read(fixture.target().join("snapshot.json")).unwrap(),
            before
        );
        assert!(
            std::fs::read_dir(fixture.workspace.join(".danmu"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("history-repair-"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangerous_export_link_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::stale();
        let snapshot = fixture.target().join("snapshot.json");
        let outside = fixture.workspace.join("outside.json");
        std::fs::write(&outside, b"outside").unwrap();
        std::fs::remove_file(&snapshot).unwrap();
        symlink(&outside, &snapshot).unwrap();
        assert!(repair_exports(&fixture.private, &fixture.workspace).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn dangerous_export_hard_link_is_rejected() {
        let fixture = Fixture::stale();
        let snapshot = fixture.target().join("snapshot.json");
        let outside = fixture.workspace.join("outside.json");
        std::fs::remove_file(&snapshot).unwrap();
        std::fs::write(&outside, &fixture.old_snapshot).unwrap();
        std::fs::hard_link(&outside, &snapshot).unwrap();
        assert!(repair_exports(&fixture.private, &fixture.workspace).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), fixture.old_snapshot);
    }

    #[test]
    fn second_run_is_idempotent_without_another_backup() {
        let fixture = Fixture::stale();
        let first = repair_exports(&fixture.private, &fixture.workspace).unwrap();
        assert!(first.backup_dir.is_some());
        let second = repair_exports(&fixture.private, &fixture.workspace).unwrap();
        assert_eq!(
            second,
            RepairReport {
                repaired_files: 0,
                backup_dir: None,
            }
        );
        let backup_count = std::fs::read_dir(fixture.workspace.join(".danmu"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("history-repair-")
            })
            .count();
        assert_eq!(backup_count, 1);
    }
}
