//! Read-only diagnostic summaries shared by terminal and native hosts.
use super::*;

pub(crate) fn account_label(status: &AccountStatus) -> String {
    match status {
        AccountStatus::SignedOut => "未登录".into(),
        AccountStatus::SignedIn {
            display_name,
            user_id,
        } => format!("{display_name} · UID {user_id}"),
    }
}
pub(crate) fn diagnostics_details(app: &RuntimeState) -> String {
    let room = app
        .runner
        .room_context()
        .map(|r| {
            format!(
                "房间 {} · 主播 {} · 标题 {}",
                r.room_id,
                r.broadcaster_id,
                if r.title.is_empty() {
                    "未获取"
                } else {
                    &r.title
                }
            )
        })
        .unwrap_or_else(|| "房间资料未建立".into());
    let routing_status = app.bridge.routing_snapshot();
    let routing = &routing_status["queue"];
    format!(
        "B站连接：{}\n主账号：{}\nOBS：{}\n配置：{}\n会话日志：{}\n\nAI状态：{} {}\n当前原因：{}\n上轮结果：{}\n场次上下文：{}\n协议：{}\n配置模型：{}\n实际返回模型：{}\n搜索状态：{}\n{}\n工作区：{}\n历史重建：{}\n历史加载：{}\n\n消息分流：模型待处理 {} / 128 · 致谢组 {}\n已交模型 {} · 代码致谢 {} · 合并 {}\n去重 {} · 过期 {} · 容量淘汰 {} · 跳过 {}\n点赞 {} · 分享 {}；分流明细见本场日志",
        app.connection,
        account_label(&app.account_status),
        app.obs_status
            .as_ref()
            .map(|s| format!(
                "场景 {} · 推流 {:?} · 麦克风 {:?}",
                s.current_scene, s.stream, s.microphone
            ))
            .unwrap_or_else(|| app.obs_error.clone().unwrap_or_else(|| "未连接".into())),
        app.storage.config_file.display(),
        app.journal.root().display(),
        app.runner.marker(),
        app.runner.state(),
        app.runner.note,
        app.runner.last_round_summary,
        app.runner.scene_id(),
        app.runner.settings.api.protocol.label(),
        app.runner.settings.api.model,
        app.runner.returned_model.as_deref().unwrap_or("无返回证据"),
        match app.runner.last_search {
            crate::runner::model::SearchState::NotRequested => "not_requested",
            crate::runner::model::SearchState::Completed => "completed",
            crate::runner::model::SearchState::Unavailable => "unavailable",
        },
        room,
        app.runner
            .workspace_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| e.to_string()),
        app.runner.history_repair_result().unwrap_or("尚未运行"),
        if app.runner.history_loading() {
            "后台载入或补齐中"
        } else {
            "未在载入"
        },
        routing["pending_models"].as_u64().unwrap_or(0),
        routing["pending_templates"].as_u64().unwrap_or(0),
        routing["model_dispatched"].as_u64().unwrap_or(0),
        routing["template_dispatched"].as_u64().unwrap_or(0),
        routing["merged"].as_u64().unwrap_or(0),
        routing["deduplicated"].as_u64().unwrap_or(0),
        routing["expired"].as_u64().unwrap_or(0),
        routing["overflow"].as_u64().unwrap_or(0),
        routing["skipped"].as_u64().unwrap_or(0),
        routing["likes"].as_u64().unwrap_or(0),
        routing["shares"].as_u64().unwrap_or(0)
    )
}
