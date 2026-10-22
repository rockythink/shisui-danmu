use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, sync::Arc, time::Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Candidate {
    pub message_id: String,
    pub text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    round_id: String,
    candidates: Vec<Candidate>,
}
#[derive(Debug, thiserror::Error)]
#[error("{kind}候选消息ID（最多80字符）：{id:?}")]
pub(crate) struct InvalidCandidateTarget {
    kind: &'static str,
    id: String,
}
pub(crate) fn candidates(text: &str, round: &str, ids: &[String]) -> Result<Vec<Candidate>> {
    let plan: Plan = serde_json::from_str(text).map_err(|_| anyhow::anyhow!("候选格式错误"))?;
    ensure!(plan.round_id == round, "候选轮次错误");
    ensure!(plan.candidates.len() <= 8, "候选数量超限");
    let mut seen = HashSet::new();
    for c in &plan.candidates {
        let kind = if !ids.contains(&c.message_id) {
            Some("未知本批")
        } else if !seen.insert(&c.message_id) {
            Some("重复本批")
        } else {
            None
        };
        if let Some(kind) = kind {
            return Err(InvalidCandidateTarget {
                kind,
                id: c.message_id.chars().take(80).collect(),
            }
            .into());
        }
        crate::bridge::Bridge::validate_reply_text(&c.text)
            .map_err(|_| anyhow::anyhow!("候选正文非法"))?;
    }
    Ok(plan.candidates)
}
#[derive(Debug, Clone)]
pub(crate) struct SearchTarget {
    pub message_id: String,
    pub query: String,
}
impl SearchTarget {
    pub(crate) fn from_public_question(message_id: &str, content: &str) -> Option<Self> {
        let query = content.trim();
        if query.is_empty() || query.chars().any(char::is_control) || query.chars().count() > 240 {
            return None;
        }
        Some(Self {
            message_id: message_id.to_owned(),
            query: query.to_owned(),
        })
    }
}
#[derive(Debug)]
pub(crate) struct Prompt {
    pub round_id: String,
    pub text: String,
    pub message_ids: Arc<[String]>,
    pub cancel_epoch: u64,
    pub deadline: Instant,
    pub web_search: bool,
    pub search_targets: Vec<SearchTarget>,
}
#[derive(Debug)]
pub(crate) enum Answer {
    Candidates(Vec<Candidate>),
    NoMessage,
    Rejected(String),
    Cancelled,
    Expired,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SearchState {
    NotRequested,
    Completed,
    Unavailable,
}
#[derive(Debug)]
pub(crate) struct RoundOutput {
    pub answer: Answer,
    pub returned_model: Option<String>,
    pub search: SearchState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn rejects_entire_batch_and_does_not_leak_private_body() {
        let ids = vec!["m1".to_owned(), "m2".to_owned()];
        let secret = "PRIVATE_SENTINEL_NEVER_LOG";
        for text in [
            secret.to_owned(),
            json!({"round_id":"r","candidates":[],secret:secret}).to_string(),
            json!({"round_id":"old","candidates":[]}).to_string(),
            json!({"round_id":"r","candidates":[{"message_id":"m1","text":"合法"},{"message_id":"m2","text":""}]}).to_string(),
        ] {
            let error = candidates(&text,"r",&ids).unwrap_err();
            assert!(!format!("{error:#}").contains(secret));
            assert!(error.downcast_ref::<serde_json::Error>().is_none());
        }
        for target in ["unknown", "m1"] {
            let text = json!({"round_id":"r","candidates":[{"message_id":"m1","text":"合法"},{"message_id":target,"text":"合法"}]}).to_string();
            assert!(
                candidates(&text, "r", &ids)
                    .unwrap_err()
                    .is::<InvalidCandidateTarget>()
            );
        }
        let nine = vec![json!({"message_id":"m1","text":"合法"}); 9];
        assert!(
            candidates(
                &json!({"round_id":"r","candidates":nine}).to_string(),
                "r",
                &ids
            )
            .is_err()
        );
        assert!(
            candidates(r#"{"round_id":"r","candidates":[]}"#, "r", &ids)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            candidates(
                r#"{"round_id":"r","candidates":[{"message_id":"m1","text":"合法"}]}"#,
                "r",
                &ids
            )
            .unwrap()[0]
                .text,
            "合法"
        );
    }
    #[test]
    fn target_diagnostic_is_bounded_and_escapes_controls() {
        let text = json!({"round_id":"r","candidates":[{"message_id":format!("\u{1b}[2J{}TAIL","字".repeat(90)),"text":"合法"}]}).to_string();
        let diagnostic = format!("{:#}", candidates(&text, "r", &[]).unwrap_err());
        assert!(!diagnostic.chars().any(char::is_control));
        assert!(!diagnostic.contains("TAIL"));
    }
    #[test]
    fn search_query_is_whole_public_question_not_truncated_or_expanded() {
        let public = " 如何理解增量游标？ ";
        let target = SearchTarget::from_public_question("current", public).unwrap();
        assert_eq!(target.query, "如何理解增量游标？");
        assert_eq!(target.message_id, "current");
        let boundary = "问".repeat(240);
        assert_eq!(
            SearchTarget::from_public_question("m", &boundary)
                .unwrap()
                .query,
            boundary
        );
        for text in [
            " ".to_owned(),
            "问".repeat(241),
            "问题\n附加内容".to_owned(),
            "问题\u{1b}[2J".to_owned(),
        ] {
            assert!(SearchTarget::from_public_question("m", &text).is_none());
        }
    }
}
