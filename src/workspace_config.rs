//! Program-owned observations, never a settings source or authorization to run/send.
use crate::runner::settings::{Settings, Source};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

const MAX_RECORD: u64 = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reading {
    pub single_line: bool,
    pub chat_layout: bool,
    pub show_time: bool,
    pub show_name: bool,
    pub history_idle_seconds: u32,
    pub theme: String,
}

// Deliberate whitelist: no Settings serialization, auth discovery, source-file reads,
// process options, runtime session IDs, room state, or Bridge permission state.
fn assistant(settings: &Settings) -> Value {
    let native = settings.native_preferences();
    json!({
        "host": settings.host,
        "name": settings.name,
        "preferences": settings.preferences,
        "use_profile": settings.use_profile,
        "use_project": settings.use_project,
        "source_kind": match settings.source { Source::Default => "default", Source::OmpProfile(_) => "omp_profile", Source::NativePrompt(_) => "native_prompt" },
        "omp_profile": match &settings.source { Source::OmpProfile(name) => Some(name), _ => None },
        "automatic_preference": settings.automatic,
        "resume_on_start_preference": settings.resume_on_start,
        "reply_activity": settings.reply_activity,
        "dynamic_host_priority": settings.dynamic_host_priority,
        "dynamic_muted_support": settings.dynamic_muted_support,
        "thank_gifts": settings.thank_gifts,
        "thank_likes": settings.thank_likes,
        "thank_follows": settings.thank_follows,
        "thank_shares": settings.thank_shares,
        "mention_sender": settings.mention_sender,
        "repair_blocked": settings.repair_blocked,
        "web_search": settings.web_search,
        "blocked_words": settings.blocked_words,
        "skills": settings.skills,
        "persona": settings.persona,
        "model": native.and_then(|p| p.model.as_deref()),
        "thinking": native.and_then(|p| p.thinking.as_deref()),
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u8,
    purpose: String,
    revision: String,
    observed_at: chrono::DateTime<chrono::Utc>,
    assistant: Value,
    reading: Option<Reading>,
}

/// Called only at startup observation or after an authoritative configuration commit.
/// The journal is committed first; current.json is an atomically replaceable projection.
/// Neither is ever deserialized into live Settings. Errors must not roll back that commit.
pub(crate) fn record(
    workspace: &Path,
    settings: &Settings,
    reading: Option<&Reading>,
) -> Result<()> {
    let directory = crate::workspace::managed_root(workspace)?.join("config");
    crate::bridge::wire::private_root(&directory)?;
    let lock = private_file(&directory.join("config.lock"))?;
    fs2::FileExt::lock_exclusive(&lock).context("锁定工作区配置审计失败")?;
    let mut history = private_file(&directory.join("history.jsonl"))?;
    let current = directory.join("current.json");
    check_file(&current)?;
    let previous = last_record(&mut history)?;
    let next_assistant = assistant(settings);
    let next_reading = reading
        .cloned()
        .or_else(|| previous.as_ref().and_then(|r| r.reading.clone()));
    let changed = previous
        .as_ref()
        .is_none_or(|r| r.assistant != next_assistant || r.reading != next_reading);
    let record = if changed {
        Record {
            schema_version: 1,
            purpose:
                "program_owned_read_only_observation_not_settings_source_or_run_send_authorization"
                    .into(),
            revision: uuid::Uuid::new_v4().to_string(),
            observed_at: chrono::Utc::now(),
            assistant: next_assistant,
            reading: next_reading,
        }
    } else {
        previous.context("缺少配置审计记录")?
    };
    if changed {
        let mut bytes = serde_json::to_vec(&record)?;
        bytes.push(b'\n');
        ensure!(bytes.len() as u64 <= MAX_RECORD, "配置审计记录过大");
        let start = history.seek(SeekFrom::End(0))?;
        if let Err(error) = history.write_all(&bytes).and_then(|()| history.sync_all()) {
            // Preserve the preceding complete journal on partial I/O failure.
            history
                .set_len(start)
                .context("审计写入失败且无法回退不完整尾部")?;
            history.sync_all()?;
            return Err(error).context("写入配置审计失败");
        }
    }
    let bytes = serde_json::to_vec_pretty(&record)?;
    // Do not touch an unchanged projection, including on reopening the application.
    let mut existing = Vec::new();
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    match options.open(&current) {
        Ok(file) => {
            ensure!(file.metadata()?.is_file(), "配置快照不是普通文件");
            file.take(MAX_RECORD + 1).read_to_end(&mut existing)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("读取配置快照失败"),
    }
    if existing != bytes {
        crate::storage::write_private_atomic(&current, &bytes)?;
    }
    Ok(())
}

fn check_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "配置审计路径必须为普通文件，不能是符号链接"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                ensure!(
                    meta.nlink() == 1
                        && meta.uid() == unsafe { libc::geteuid() }
                        && meta.mode() & 0o077 == 0,
                    "配置审计文件必须由当前用户独占且仅本人可访问"
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("检查配置审计文件失败"),
    }
}
fn private_file(path: &Path) -> Result<File> {
    check_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    ensure!(file.metadata()?.is_file(), "配置审计不是普通文件");
    Ok(file)
}
fn last_record(file: &mut File) -> Result<Option<Record>> {
    let length = file.metadata()?.len();
    if length == 0 {
        return Ok(None);
    }
    let start = length.saturating_sub(MAX_RECORD + 1);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    file.take(MAX_RECORD + 1).read_to_end(&mut tail)?;
    // A crash during append leaves an unterminated tail. Discard only that tail,
    // never a complete malformed record (which requires operator repair).
    let end = tail
        .iter()
        .rposition(|b| *b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    ensure!(end > 0 || start == 0, "配置审计尾部超过安全读取上限");
    if end < tail.len() {
        file.set_len(start + end as u64)?;
        file.sync_all()?;
    }
    if end == 0 {
        return Ok(None);
    }
    let begin = tail[..end - 1]
        .iter()
        .rposition(|b| *b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    ensure!(begin > 0 || start == 0, "配置审计记录超过安全读取上限");
    let record: Record =
        serde_json::from_slice(&tail[begin..end - 1]).context("配置审计尾记录损坏，未覆盖")?;
    ensure!(record.schema_version == 1, "未知配置审计版本，未覆盖");
    Ok(Some(record))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn history(root: &Path) -> Vec<Value> {
        std::fs::read_to_string(root.join(".danmu/config/history.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
    #[test]
    fn unchanged_reopen_and_aba_preserve_real_changes() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        record(root.path(), &settings, None).unwrap();
        let original = std::fs::read(root.path().join(".danmu/config/current.json")).unwrap();
        record(root.path(), &settings, None).unwrap();
        assert_eq!(history(root.path()).len(), 1);
        assert_eq!(
            std::fs::read(root.path().join(".danmu/config/current.json")).unwrap(),
            original
        );
        settings.preferences = "简短回答".into();
        record(root.path(), &settings, None).unwrap();
        settings.preferences.clear();
        record(root.path(), &settings, None).unwrap();
        let records = history(root.path());
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["assistant"], records[2]["assistant"]);
        assert_ne!(records[0]["revision"], records[2]["revision"]);
    }
    #[test]
    fn source_paths_and_contents_are_not_copied() {
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("native-auth-secret.json");
        std::fs::write(&secret, "BILIBILI_OBS_MODEL_BRIDGE_SECRET").unwrap();
        let settings = Settings {
            source: Source::NativePrompt(secret),
            binary: "/private/SECRET_EXECUTABLE".into(),
            ..Settings::default()
        };
        record(root.path(), &settings, None).unwrap();
        let text = std::fs::read_to_string(root.path().join(".danmu/config/current.json")).unwrap();
        assert!(!text.contains("SECRET") && !text.contains("native-auth-secret"));
        assert_eq!(
            history(root.path())[0]["assistant"]["source_kind"],
            "native_prompt"
        );
    }
    #[test]
    fn committed_journal_repairs_missing_projection_and_partial_tail() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        record(root.path(), &settings, None).unwrap();
        let current = root.path().join(".danmu/config/current.json");
        std::fs::remove_file(&current).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(root.path().join(".danmu/config/history.jsonl"))
            .unwrap();
        file.write_all(b"{\"partial\":").unwrap();
        record(root.path(), &settings, None).unwrap();
        let restored: Value = serde_json::from_slice(&std::fs::read(current).unwrap()).unwrap();
        assert_eq!(history(root.path()), vec![restored]);
    }
    #[test]
    fn concurrent_identical_observations_append_once() {
        let root = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| record(root.path(), &Settings::default(), None).unwrap());
            }
        });
        assert_eq!(history(root.path()).len(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn linked_audit_files_and_directory_never_touch_target() {
        use std::os::unix::fs::symlink;
        for name in ["config.lock", "history.jsonl", "current.json"] {
            let root = tempfile::tempdir().unwrap();
            let directory = crate::workspace::managed_root(root.path())
                .unwrap()
                .join("config");
            crate::bridge::wire::private_root(&directory).unwrap();
            let outside = root.path().join("outside");
            std::fs::write(&outside, "private secret").unwrap();
            symlink(&outside, directory.join(name)).unwrap();
            assert!(record(root.path(), &Settings::default(), None).is_err());
            assert_eq!(std::fs::read_to_string(outside).unwrap(), "private secret");
        }
        let root = tempfile::tempdir().unwrap();
        let managed = crate::workspace::managed_root(root.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), managed.join("config")).unwrap();
        assert!(record(root.path(), &Settings::default(), None).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
    #[test]
    fn reading_and_native_selection_survive_assistant_only_updates() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        let mut native = crate::runner::settings::NativePreferences::scope(&settings);
        native.model = Some("chosen-model".into());
        native.thinking = Some("high".into());
        settings.remember_native(native);
        let reading = Reading {
            single_line: true,
            chat_layout: false,
            show_time: true,
            show_name: false,
            history_idle_seconds: 17,
            theme: "light".into(),
        };
        record(root.path(), &settings, Some(&reading)).unwrap();
        settings.dynamic_host_priority = false;
        record(root.path(), &settings, None).unwrap();
        let current: Value = serde_json::from_slice(
            &std::fs::read(root.path().join(".danmu/config/current.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(current["reading"], serde_json::to_value(&reading).unwrap());
        assert_eq!(current["assistant"]["model"], "chosen-model");
        assert_eq!(current["assistant"]["thinking"], "high");
        assert_eq!(current["assistant"]["dynamic_host_priority"], false);
        assert_eq!(history(root.path()).len(), 2);
    }
    #[test]
    fn audit_failure_does_not_undo_committed_settings_or_claim_failure() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        let workspace = root.path().join("workspace");
        let mut runner = crate::runner::Runner::load(&config);
        runner.settings.workspace = Some(workspace.clone());
        let directory = crate::workspace::managed_root(&workspace)
            .unwrap()
            .join("config");
        crate::bridge::wire::private_root(&directory).unwrap();
        std::fs::create_dir(directory.join("current.json")).unwrap();
        let mut settings = runner.settings.clone();
        settings.preferences = "已正式提交".into();
        runner
            .save(settings.clone(), &crate::bridge::Bridge::new(true), false)
            .unwrap();
        let (saved, _) = Settings::load(&root.path().join("assistant.json")).unwrap();
        assert_eq!(runner.settings, saved);
        assert_eq!(saved.preferences, settings.preferences);
        assert!(runner.config_warning.is_some());
        assert!(!runner.running);
        std::fs::remove_dir(directory.join("current.json")).unwrap();
        runner
            .save(saved, &crate::bridge::Bridge::new(true), false)
            .unwrap();
        assert!(runner.config_warning.is_none());
        assert_eq!(history(&workspace).len(), 1);
        let mut invalid = runner.settings.clone();
        invalid.name.clear();
        assert!(
            runner
                .save(invalid, &crate::bridge::Bridge::new(true), false)
                .is_err()
        );
        assert_eq!(history(&workspace).len(), 1);
    }
}
