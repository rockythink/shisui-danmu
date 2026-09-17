use super::Message;
use crate::domain::DanmuEventKind;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

pub(crate) const MAX_AGE: Duration = Duration::from_secs(120);
const MODEL_CAPACITY: usize = 128;
const MODEL_BYTES: usize = 256 * 1024;
const MODEL_BATCH: usize = 8;
const PRIORITY_BATCH: usize = 6;
const NORMAL_BATCH: usize = 2;
const DEDUP_CAPACITY: usize = 1024;
const DEDUP_AGE: Duration = Duration::from_secs(30);
const FOLLOW_DEDUP_AGE: Duration = Duration::from_secs(300);
const TEMPLATE_BATCH: Duration = Duration::from_secs(5);
const TEMPLATE_GAP: Duration = Duration::from_secs(15);
const RECENT_DECISIONS: usize = 64;
const PERSONAL_THANKS_LIMIT: usize = 5;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Policy {
    pub author: Option<String>,
    pub name: String,
    pub thank_gifts: bool,
    pub thank_likes: bool,
    pub thank_follows: bool,
    pub thank_shares: bool,
}

#[derive(Clone)]
pub(crate) struct Template {
    pub message: Message,
    pub text: String,
    pub mention_sender: bool,
}

#[derive(Clone)]
struct QueuedModel {
    message: Message,
    received_at: Instant,
    bytes: usize,
    priority: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TemplateKind {
    Follow,
    Gift,
    Guard,
    Like,
    Share,
}

impl TemplateKind {
    const ALL: [Self; 5] = [
        Self::Follow,
        Self::Gift,
        Self::Guard,
        Self::Like,
        Self::Share,
    ];

    fn index(self) -> usize {
        match self {
            Self::Follow => 0,
            Self::Gift => 1,
            Self::Guard => 2,
            Self::Like => 3,
            Self::Share => 4,
        }
    }

    fn cooldown(self) -> Duration {
        match self {
            Self::Follow => Duration::from_secs(30),
            Self::Gift | Self::Guard => Duration::from_secs(15),
            Self::Like | Self::Share => Duration::from_secs(60),
        }
    }

    fn text(self, variant: usize, collective: bool) -> &'static str {
        let (personal, group) = match self {
            Self::Follow => (
                [
                    "谢谢关注，欢迎常来！",
                    "感谢关注，一起交流！",
                    "收到关注啦，欢迎你！",
                ],
                [
                    "感谢大家的关注，欢迎常来！",
                    "谢谢新关注的朋友们！",
                    "欢迎新朋友，一起交流！",
                ],
            ),
            Self::Gift => (
                [
                    "谢谢你的礼物支持！",
                    "礼物收到啦，谢谢你！",
                    "感谢送来的礼物，心意收到！",
                ],
                [
                    "感谢大家的礼物支持！",
                    "收到大家的礼物啦，谢谢！",
                    "谢谢送礼物的朋友们！",
                ],
            ),
            Self::Guard => (
                [
                    "感谢上舰，欢迎加入！",
                    "谢谢你的上舰支持！",
                    "欢迎登舰，谢谢支持！",
                ],
                [
                    "感谢大家的上舰支持！",
                    "欢迎新上舰的朋友们！",
                    "谢谢大家登舰支持！",
                ],
            ),
            Self::Like => (
                ["谢谢你的点赞！", "收到你的赞啦，谢谢！", "感谢点赞支持！"],
                [
                    "谢谢大家的点赞！",
                    "收到大家的赞啦，谢谢！",
                    "感谢点赞的朋友们！",
                ],
            ),
            Self::Share => (
                [
                    "谢谢你分享直播间！",
                    "感谢分享，欢迎朋友来聊！",
                    "谢谢帮忙分享直播！",
                ],
                [
                    "感谢大家分享直播间！",
                    "谢谢帮忙分享的朋友们！",
                    "收到大家的分享支持，谢谢！",
                ],
            ),
        };
        if collective {
            group[variant]
        } else {
            personal[variant]
        }
    }
}

#[derive(Clone)]
struct TemplateSlot {
    message: Message,
    first_received_at: Instant,
    others: VecDeque<Message>,
    collective: bool,
}

struct DedupEntry {
    expires_at: Instant,
}

#[derive(Default)]
struct Counters {
    model_dispatched: u64,
    template_dispatched: u64,
    skipped: u64,
    deduplicated: u64,
    merged: u64,
    expired: u64,
    overflow: u64,
    likes: u64,
    shares: u64,
}

struct Decision {
    message_id: String,
    reason: String,
}

pub(crate) struct Inbox {
    policy: Policy,
    models: VecDeque<QueuedModel>,
    model_bytes: usize,
    templates: [Option<TemplateSlot>; 5],
    active_template: Option<(TemplateKind, TemplateSlot)>,
    template_variant: [usize; 5],
    dedup: HashMap<[u8; 32], DedupEntry>,
    dedup_order: VecDeque<([u8; 32], Instant)>,
    last_template: Option<Instant>,
    kind_last_template: [Option<Instant>; 5],
    counters: Counters,
    recent_decisions: VecDeque<Decision>,
}

impl Inbox {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            models: VecDeque::new(),
            model_bytes: 0,
            templates: std::array::from_fn(|_| None),
            active_template: None,
            template_variant: [0; 5],
            dedup: HashMap::new(),
            dedup_order: VecDeque::new(),
            last_template: None,
            kind_last_template: [None; 5],
            counters: Counters::default(),
            recent_decisions: VecDeque::new(),
        }
    }

    pub fn configure(&mut self, policy: Policy) {
        if self.policy == policy {
            return;
        }
        self.policy = policy;

        let mut kept = VecDeque::with_capacity(self.models.len());
        while let Some(mut queued) = self.models.pop_front() {
            if self.is_self(&queued.message) {
                self.model_bytes = self.model_bytes.saturating_sub(queued.bytes);
                self.counters.skipped += 1;
                self.decision(&queued.message.event.id, "configured_self");
            } else {
                queued.priority = self.is_priority(&queued.message);
                kept.push_back(queued);
            }
        }
        self.models = kept;

        if self
            .active_template
            .as_ref()
            .is_some_and(|(kind, slot)| !self.template_enabled(*kind) || self.slot_is_self(slot))
        {
            let (_, slot) = self.active_template.take().unwrap();
            self.counters.skipped += 1;
            self.decision(&slot.message.event.id, "configured_discarded");
        }
        for kind in TemplateKind::ALL {
            if !self.template_enabled(kind) {
                if let Some(slot) = self.templates[kind.index()].take() {
                    self.counters.skipped += 1;
                    self.decision(&slot.message.event.id, "configured_disabled");
                }
            } else if self.templates[kind.index()]
                .as_ref()
                .is_some_and(|slot| self.slot_is_self(slot))
            {
                let slot = self.templates[kind.index()]
                    .take()
                    .expect("slot was present");
                self.counters.skipped += 1;
                self.decision(&slot.message.event.id, "configured_self");
            }
        }
    }

    pub fn push(&mut self, message: &Message, now: Instant) {
        self.prune_dedup(now);
        if self.original_expired(message) {
            self.counters.expired += 1;
            self.decision(&message.event.id, "expired_at_ingest");
            return;
        }
        if self.is_self(message) {
            self.counters.skipped += 1;
            self.decision(&message.event.id, "self");
            return;
        }

        let supported = matches!(
            message.event.kind,
            DanmuEventKind::Danmu
                | DanmuEventKind::Superchat
                | DanmuEventKind::Gift
                | DanmuEventKind::GuardEvent
                | DanmuEventKind::Like
                | DanmuEventKind::Follow
                | DanmuEventKind::Share
        );
        if !supported {
            self.counters.skipped += 1;
            self.decision(&message.event.id, "unsupported_kind");
            return;
        }
        match message.event.kind {
            DanmuEventKind::Like => self.counters.likes += 1,
            DanmuEventKind::Share => self.counters.shares += 1,
            _ => {}
        }
        if message.event.kind == DanmuEventKind::Danmu
            && !self.is_priority(message)
            && is_pure_reaction(message)
        {
            self.counters.skipped += 1;
            self.decision(&message.event.id, "model_reaction_skipped");
            return;
        }
        if self.is_duplicate(message, now) {
            self.counters.deduplicated += 1;
            self.decision(&message.event.id, "deduplicated");
            return;
        }

        match message.event.kind {
            DanmuEventKind::Danmu | DanmuEventKind::Superchat => self.push_model(message, now),
            DanmuEventKind::Follow => self.push_template(TemplateKind::Follow, message, now),
            DanmuEventKind::Gift => self.push_template(TemplateKind::Gift, message, now),
            DanmuEventKind::GuardEvent => self.push_template(TemplateKind::Guard, message, now),
            DanmuEventKind::Like => self.push_template(TemplateKind::Like, message, now),
            DanmuEventKind::Share => self.push_template(TemplateKind::Share, message, now),
            _ => unreachable!("supported kinds are exhausted"),
        }
    }

    pub fn models(&mut self, now: Instant) -> Vec<Message> {
        self.expire_models(now);
        let mut selected = Vec::with_capacity(MODEL_BATCH);
        let priority_count = self.extend_models(&mut selected, true, 0, PRIORITY_BATCH);
        let normal_count = self.extend_models(&mut selected, false, 0, NORMAL_BATCH);
        if selected.len() < MODEL_BATCH {
            let remaining = MODEL_BATCH - selected.len();
            let added = self.extend_models(&mut selected, true, priority_count, remaining);
            if added < remaining {
                self.extend_models(&mut selected, false, normal_count, remaining - added);
            }
        }
        selected
    }

    pub fn has_priority(&self) -> bool {
        self.models.iter().any(|queued| queued.priority)
    }

    pub fn maintain(&mut self, now: Instant) {
        self.expire_models(now);
        self.expire_templates(now);
        self.prune_dedup(now);
    }

    pub fn consume_models(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        let mut index = 0;
        while index < self.models.len() {
            if ids.contains(&self.models[index].message.event.id) {
                let queued = self
                    .models
                    .remove(index)
                    .expect("located model was present");
                self.model_bytes -= queued.bytes;
                self.counters.model_dispatched += 1;
                self.decision(&queued.message.event.id, "model_dispatched");
            } else {
                index += 1;
            }
        }
    }

    pub fn template(&mut self, now: Instant) -> Option<Template> {
        self.expire_templates(now);
        if self
            .last_template
            .is_some_and(|last| elapsed(now, last) < TEMPLATE_GAP)
        {
            return None;
        }
        if self.active_template.is_none() {
            let kind = TemplateKind::ALL
                .into_iter()
                .filter_map(|kind| {
                    let slot = self.templates[kind.index()].as_ref()?;
                    if elapsed(now, slot.first_received_at) < TEMPLATE_BATCH
                        || self.kind_last_template[kind.index()]
                            .is_some_and(|last| elapsed(now, last) < kind.cooldown())
                    {
                        return None;
                    }
                    Some((kind, slot.first_received_at))
                })
                .min_by_key(|(kind, received)| (*received, kind.index()))?
                .0;
            // Freeze this batch before dispatch; later arrivals form a separate batch.
            self.active_template = Some((kind, self.templates[kind.index()].take().unwrap()));
        }
        let (kind, slot) = self.active_template.as_ref()?;
        Some(Template {
            message: slot.message.clone(),
            text: kind
                .text(self.template_variant[kind.index()], slot.collective)
                .to_owned(),
            mention_sender: !slot.collective,
        })
    }

    pub fn consume_template(&mut self, id: &str, now: Instant) {
        let Some((kind, slot)) = self.active_template.as_mut() else {
            return;
        };
        if slot.message.event.id != id {
            return;
        }
        let kind = *kind;
        let finished = if let Some(next) = slot.others.pop_front() {
            slot.message = next;
            false
        } else {
            true
        };
        self.last_template = Some(now);
        self.template_variant[kind.index()] = (self.template_variant[kind.index()] + 1) % 3;
        if finished {
            self.active_template = None;
            self.kind_last_template[kind.index()] = Some(now);
        }
        self.counters.template_dispatched += 1;
        self.decision(id, "template_dispatched");
    }

    pub fn clear(&mut self) {
        while let Some(queued) = self.models.pop_front() {
            self.counters.skipped += 1;
            self.decision(&queued.message.event.id, "paused/discarded");
        }
        self.model_bytes = 0;
        if let Some((_, slot)) = self.active_template.take() {
            self.counters.skipped += 1;
            self.decision(&slot.message.event.id, "paused/discarded");
        }
        for kind in TemplateKind::ALL {
            if let Some(slot) = self.templates[kind.index()].take() {
                self.counters.skipped += 1;
                self.decision(&slot.message.event.id, "paused/discarded");
            }
        }
        self.dedup.clear();
        self.dedup_order.clear();
        self.last_template = None;
        self.kind_last_template = [None; 5];
    }

    pub fn snapshot(&self) -> Value {
        json!({
            "pending_models": self.models.len(),
            "pending_templates": self.templates.iter().flatten().count() + usize::from(self.active_template.is_some()),
            "model_dispatched": self.counters.model_dispatched,
            "template_dispatched": self.counters.template_dispatched,
            "skipped": self.counters.skipped,
            "deduplicated": self.counters.deduplicated,
            "merged": self.counters.merged,
            "expired": self.counters.expired,
            "overflow": self.counters.overflow,
            "likes": self.counters.likes,
            "shares": self.counters.shares,
            "recent_decisions": self.recent_decisions.iter().map(|decision| json!({
                "message_id": decision.message_id,
                "reason": decision.reason,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn get(&self, id: &str) -> Option<&Message> {
        self.models
            .iter()
            .find(|queued| queued.message.event.id == id)
            .map(|queued| &queued.message)
            .or_else(|| {
                self.templates
                    .iter()
                    .flatten()
                    .chain(self.active_template.iter().map(|(_, slot)| slot))
                    .flat_map(|slot| std::iter::once(&slot.message).chain(slot.others.iter()))
                    .find(|message| message.event.id == id)
            })
    }

    pub fn drop_model(&mut self, id: &str, reason: &str) {
        let Some(index) = self
            .models
            .iter()
            .position(|queued| queued.message.event.id == id)
        else {
            return;
        };
        let queued = self
            .models
            .remove(index)
            .expect("located model was present");
        self.model_bytes = self.model_bytes.saturating_sub(queued.bytes);
        self.counters.skipped += 1;
        self.decision(&queued.message.event.id, reason);
    }

    pub fn record_expired(&mut self, ids: &[String]) {
        for id in ids {
            self.counters.expired += 1;
            self.decision(id, "expired_in_flight");
        }
    }
    fn push_model(&mut self, message: &Message, now: Instant) {
        self.expire_models(now);
        let bytes = match encoded_size(message) {
            Ok(size) => size.saturating_add(1),
            Err(_) => {
                self.counters.skipped += 1;
                self.decision(&message.event.id, "serialization_failed");
                return;
            }
        };
        if bytes > MODEL_BYTES.saturating_sub(1) {
            self.counters.skipped += 1;
            self.counters.overflow += 1;
            self.decision(&message.event.id, "oversized");
            return;
        }
        let priority = self.is_priority(message);
        while self.models.len() >= MODEL_CAPACITY
            || self.model_bytes.saturating_add(bytes) > MODEL_BYTES.saturating_sub(1)
        {
            let victim = self
                .models
                .iter()
                .position(|queued| !queued.priority)
                .or_else(|| priority.then_some(0));
            let Some(victim) = victim else {
                self.counters.skipped += 1;
                self.counters.overflow += 1;
                self.decision(&message.event.id, "overflow_priority_preserved");
                return;
            };
            let evicted = self
                .models
                .remove(victim)
                .expect("located victim was present");
            self.model_bytes = self.model_bytes.saturating_sub(evicted.bytes);
            self.counters.overflow += 1;
            self.decision(&evicted.message.event.id, "overflow_evicted");
        }
        self.model_bytes += bytes;
        self.models.push_back(QueuedModel {
            message: message.clone(),
            received_at: now,
            bytes,
            priority,
        });
        self.decision(
            &message.event.id,
            if priority {
                "model_priority"
            } else {
                "model_normal"
            },
        );
    }

    fn push_template(&mut self, kind: TemplateKind, message: &Message, now: Instant) {
        self.expire_templates(now);
        if !self.template_enabled(kind) {
            self.counters.skipped += 1;
            self.decision(&message.event.id, "template_disabled");
            return;
        }
        if viewer_identity(message).0 == 2 {
            self.counters.skipped += 1;
            self.decision(&message.event.id, "template_missing_recipient");
            return;
        }
        if !encoded_size(message).is_ok_and(|bytes| bytes <= 12 * 1024) {
            self.counters.skipped += 1;
            self.decision(&message.event.id, "oversized_template_source");
            return;
        }
        let index = kind.index();
        if let Some(slot) = self.templates[index].as_mut() {
            if !slot.collective
                && !std::iter::once(&slot.message)
                    .chain(slot.others.iter())
                    .any(|existing| viewer_identity(existing) == viewer_identity(message))
            {
                if slot.others.len() + 1 < PERSONAL_THANKS_LIMIT {
                    slot.others.push_back(message.clone());
                } else {
                    slot.collective = true;
                    slot.others.clear();
                }
            }
            self.counters.merged += 1;
            self.decision(&message.event.id, "template_merged");
        } else {
            self.templates[index] = Some(TemplateSlot {
                message: message.clone(),
                first_received_at: now,
                others: VecDeque::new(),
                collective: false,
            });
            self.decision(&message.event.id, "template_pending");
        }
    }

    fn extend_models(
        &self,
        output: &mut Vec<Message>,
        priority: bool,
        skip: usize,
        amount: usize,
    ) -> usize {
        let before = output.len();
        output.extend(
            self.models
                .iter()
                .filter(|queued| queued.priority == priority)
                .skip(skip)
                .take(amount)
                .map(|queued| queued.message.clone()),
        );
        output.len() - before
    }

    fn expire_models(&mut self, now: Instant) {
        let mut index = 0;
        let wall_now = chrono::Utc::now();
        while index < self.models.len() {
            let queued = &self.models[index];
            if elapsed(now, queued.received_at) >= MAX_AGE
                || (wall_now - queued.message.event.timestamp)
                    .to_std()
                    .is_ok_and(|age| age >= MAX_AGE)
            {
                let queued = self
                    .models
                    .remove(index)
                    .expect("located model was present");
                self.model_bytes -= queued.bytes;
                self.counters.expired += 1;
                self.decision(&queued.message.event.id, "expired");
            } else {
                index += 1;
            }
        }
    }

    fn expire_templates(&mut self, now: Instant) {
        if self.active_template.as_ref().is_some_and(|(_, slot)| {
            elapsed(now, slot.first_received_at) >= MAX_AGE || self.original_expired(&slot.message)
        }) {
            let (_, slot) = self.active_template.take().unwrap();
            self.counters.expired += 1;
            self.decision(&slot.message.event.id, "expired");
        }
        for kind in TemplateKind::ALL {
            let expired = self.templates[kind.index()].as_ref().is_some_and(|slot| {
                elapsed(now, slot.first_received_at) >= MAX_AGE
                    || self.original_expired(&slot.message)
            });
            if expired {
                let slot = self.templates[kind.index()]
                    .take()
                    .expect("expired slot was present");
                self.counters.expired += 1;
                self.decision(&slot.message.event.id, "expired");
            }
        }
    }

    fn is_priority(&self, message: &Message) -> bool {
        let name = self.policy.name.trim();
        if name.is_empty() {
            return false;
        }
        message.event.reply_to.as_deref() == Some(name)
            || message.event.content.match_indices('@').any(|(index, _)| {
                message.event.content[index + 1..]
                    .strip_prefix(name)
                    .is_some_and(is_mention_boundary)
            })
            || message
                .event
                .content
                .trim_start()
                .strip_prefix(name)
                .is_some_and(|rest| rest.chars().next().is_some_and(is_direct_separator))
    }

    fn is_self(&self, message: &Message) -> bool {
        message.event.author_id.as_deref() == Some("local-host")
            || self
                .policy
                .author
                .as_deref()
                .filter(|author| !author.trim().is_empty() && *author != "0")
                .is_some_and(|author| message.event.author_id.as_deref() == Some(author))
    }

    fn slot_is_self(&self, slot: &TemplateSlot) -> bool {
        std::iter::once(&slot.message)
            .chain(slot.others.iter())
            .any(|message| self.is_self(message))
    }

    fn template_enabled(&self, kind: TemplateKind) -> bool {
        match kind {
            TemplateKind::Follow => self.policy.thank_follows,
            TemplateKind::Gift | TemplateKind::Guard => self.policy.thank_gifts,
            TemplateKind::Like => self.policy.thank_likes,
            TemplateKind::Share => self.policy.thank_shares,
        }
    }

    fn original_expired(&self, message: &Message) -> bool {
        chrono::Utc::now()
            .signed_duration_since(message.event.timestamp)
            .to_std()
            .is_ok_and(|age| age >= MAX_AGE)
    }

    fn is_duplicate(&mut self, message: &Message, now: Instant) -> bool {
        let (identity_kind, author) = viewer_identity(message);
        let mut digest = Sha256::new();
        digest.update([identity_kind, message.event.kind as u8]);
        for part in [
            author,
            if message.event.kind == DanmuEventKind::Follow {
                ""
            } else {
                message.event.content.trim()
            },
            if message.event.kind == DanmuEventKind::Follow {
                ""
            } else {
                message.event.reply_to.as_deref().unwrap_or("")
            },
        ] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part.as_bytes());
        }
        let key: [u8; 32] = digest.finalize().into();
        if self
            .dedup
            .get(&key)
            .is_some_and(|entry| now < entry.expires_at)
        {
            return true;
        }
        let lifetime = if message.event.kind == DanmuEventKind::Follow {
            FOLLOW_DEDUP_AGE
        } else {
            DEDUP_AGE
        };
        let expires_at = now.checked_add(lifetime).unwrap_or(now);
        self.dedup.insert(key, DedupEntry { expires_at });
        self.dedup_order.push_back((key, expires_at));
        self.prune_dedup(now);
        false
    }

    fn prune_dedup(&mut self, now: Instant) {
        while let Some((key, expires_at)) = self.dedup_order.front() {
            if *expires_at > now && self.dedup_order.len() <= DEDUP_CAPACITY {
                break;
            }
            let key = *key;
            let expires_at = *expires_at;
            self.dedup_order.pop_front();
            if self
                .dedup
                .get(&key)
                .is_some_and(|entry| entry.expires_at == expires_at)
            {
                self.dedup.remove(&key);
            }
        }
    }

    fn decision(&mut self, message_id: &str, reason: &str) {
        if self.recent_decisions.len() == RECENT_DECISIONS {
            self.recent_decisions.pop_front();
        }
        self.recent_decisions.push_back(Decision {
            message_id: message_id.to_owned(),
            reason: reason.to_owned(),
        });
    }
}

fn is_mention_boundary(rest: &str) -> bool {
    rest.chars()
        .next()
        .is_none_or(|ch| ch.is_whitespace() || is_reaction_punctuation(ch))
}

fn is_direct_separator(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, ',' | '，' | ':' | '：')
}

// Classify only reactions whose complete body is accounted for. Emote tokens come from the
// parsed event, so arbitrary bracketed text never becomes an emote by convention.
fn is_pure_reaction(message: &Message) -> bool {
    let mut rest = message.event.content.as_str();
    if rest.trim().is_empty() {
        return true;
    }

    let mut ascii_laughs = 0;
    let mut chinese_laughs = 0;
    let mut saw_emote = false;
    while !rest.is_empty() {
        if let Some(text) = message
            .event
            .emotes
            .iter()
            .map(|emote| emote.text.as_str())
            .find(|text| !text.is_empty() && rest.starts_with(text))
        {
            saw_emote = true;
            rest = &rest[text.len()..];
            continue;
        }

        let ch = rest.chars().next().expect("rest is non-empty");
        rest = &rest[ch.len_utf8()..];
        match ch {
            'h' | 'H' => ascii_laughs += 1,
            '哈' => chinese_laughs += 1,
            _ if ch.is_whitespace() || is_reaction_punctuation(ch) => {}
            _ => return false,
        }
    }

    saw_emote || ascii_laughs >= 3 || chinese_laughs >= 2
}

fn is_reaction_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation()
        || matches!(
            ch,
            '，' | '。'
                | '！'
                | '？'
                | '：'
                | '；'
                | '、'
                | '…'
                | '～'
                | '—'
                | '（'
                | '）'
                | '【'
                | '】'
                | '《'
                | '》'
                | '“'
                | '”'
                | '‘'
                | '’'
        )
}

// Count JSON bytes without allocating a second copy of every incoming body.
fn encoded_size(message: &Message) -> serde_json::Result<usize> {
    struct Size(usize);
    impl std::io::Write for Size {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut size = Size(0);
    serde_json::to_writer(&mut size, message)?;
    Ok(size.0)
}

fn viewer_identity(message: &Message) -> (u8, &str) {
    if let Some(id) = message
        .event
        .author_id
        .as_deref()
        .filter(|id| !id.trim().is_empty() && *id != "0")
    {
        (0, id)
    } else if let Some(name) = message
        .event
        .username
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        (1, name)
    } else {
        // An anonymous event is not evidence that two viewers are the same person.
        (2, &message.event.id)
    }
}

fn elapsed(now: Instant, then: Instant) -> Duration {
    now.checked_duration_since(then).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DanmuEmote, DanmuEvent, DanmuEventKind};

    fn policy() -> Policy {
        Policy {
            author: Some("host-id".into()),
            name: "主播".into(),
            thank_gifts: true,
            thank_likes: true,
            thank_follows: true,
            thank_shares: true,
        }
    }

    fn message(id: &str, author: &str, kind: DanmuEventKind, content: &str) -> Message {
        let mut event = DanmuEvent::new(kind, content);
        event.id = id.into();
        event.author_id = Some(author.into());
        event.username = Some(format!("user-{author}"));
        Message { cursor: 0, event }
    }
    #[test]
    fn pure_reactions_are_skipped_without_hiding_substantive_or_targeted_messages() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        for (id, content) in [
            ("blank", "  \t"),
            ("ascii-laugh", "hhh!!!"),
            ("chinese-laugh", "哈哈哈哈～"),
        ] {
            inbox.push(&message(id, id, DanmuEventKind::Danmu, content), now);
        }

        let emote = DanmuEmote {
            text: "[dog]".into(),
            fallback: "🙂".into(),
            image_url: url::Url::parse("https://example.com/dog.png").unwrap(),
            width: None,
            height: None,
            is_animated: false,
        };
        let mut emote_only = message("emote-only", "emote", DanmuEventKind::Danmu, "[dog]！");
        emote_only.event.emotes.push(emote.clone());
        inbox.push(&emote_only, now);

        let mut mixed = message("mixed", "mixed", DanmuEventKind::Danmu, "这个[dog]很可爱");
        mixed.event.emotes.push(emote);
        inbox.push(&mixed, now);
        for (id, author, content) in [
            ("unrecognized", "plain", "[dog]"),
            ("short", "short", "h"),
            ("question-mark", "question-mark", "？"),
            ("punctuation", "punctuation", "..."),
            ("question", "question", "能讲讲吗？"),
            ("mixed-laugh", "praise", "哈哈，讲得真好"),
            ("mention", "mention", "@主播 hhh"),
            ("direct", "direct", "主播，hhh"),
        ] {
            inbox.push(&message(id, author, DanmuEventKind::Danmu, content), now);
        }
        let mut reply = message("reply", "reply", DanmuEventKind::Danmu, "哈哈");
        reply.event.reply_to = Some("主播".into());
        inbox.push(&reply, now);
        inbox.push(
            &message("superchat", "sc", DanmuEventKind::Superchat, "哈哈哈"),
            now,
        );

        let ids = inbox
            .models(now)
            .into_iter()
            .map(|message| message.event.id)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "mention",
                "direct",
                "reply",
                "mixed",
                "unrecognized",
                "short",
                "question-mark",
                "punctuation",
            ]
        );
        for id in [
            "mixed",
            "unrecognized",
            "short",
            "question-mark",
            "punctuation",
            "question",
            "mixed-laugh",
            "mention",
            "direct",
            "reply",
            "superchat",
        ] {
            assert!(inbox.get(id).is_some(), "{id} must remain queued");
        }
        for id in ["blank", "ascii-laugh", "chinese-laugh", "emote-only"] {
            assert!(inbox.get(id).is_none(), "{id} must be filtered");
        }
        assert!(inbox.has_priority());
        assert_eq!(inbox.snapshot()["pending_models"], 11);
        assert_eq!(inbox.snapshot()["skipped"], 4);
        let decisions = inbox.snapshot()["recent_decisions"].clone();
        for id in ["blank", "ascii-laugh", "chinese-laugh", "emote-only"] {
            assert!(decisions.as_array().unwrap().iter().any(|decision| {
                decision["message_id"] == id && decision["reason"] == "model_reaction_skipped"
            }));
        }
        assert_eq!(inbox.snapshot()["pending_models"], 11);
    }

    #[test]
    fn priority_requires_an_explicit_name_boundary_and_does_not_consume() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        for (id, content) in [
            ("at", "@主播 能解释吗"),
            ("at-punctuation", "@主播？"),
            ("direct-space", "主播 讲讲"),
            ("direct-comma", "主播，讲讲"),
            ("direct-colon", "主播：讲讲"),
        ] {
            inbox.push(&message(id, id, DanmuEventKind::Danmu, content), now);
        }
        for (id, content) in [
            ("longer-at", "@主播甲 你好"),
            ("longer-prefix", "主播间有什么区别"),
            ("middle", "我觉得主播，讲得好"),
        ] {
            inbox.push(&message(id, id, DanmuEventKind::Danmu, content), now);
        }

        assert!(inbox.has_priority());
        assert!(inbox.has_priority());
        assert_eq!(inbox.snapshot()["pending_models"], 8);
        assert_eq!(
            inbox
                .models(now)
                .iter()
                .take_while(|message| message.event.id != "longer-at")
                .count(),
            5
        );
    }

    #[test]
    fn identity_filter_does_not_drop_namesakes_or_merge_unknown_uids() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        let mut namesake = message("namesake", "viewer", DanmuEventKind::Danmu, "正文");
        namesake.event.username = Some("主播".into());
        inbox.push(&namesake, now);
        for name in ["甲", "乙"] {
            let mut unknown = message(name, "0", DanmuEventKind::Danmu, "相同问题");
            unknown.event.username = Some(name.into());
            inbox.push(&unknown, now);
        }
        inbox.push(
            &message("own", "host-id", DanmuEventKind::Danmu, "自身"),
            now,
        );
        inbox.push(
            &message("local", "local-host", DanmuEventKind::Danmu, "本机"),
            now,
        );
        assert_eq!(
            inbox
                .models(now)
                .iter()
                .map(|m| m.event.id.as_str())
                .collect::<Vec<_>>(),
            ["namesake", "甲", "乙"]
        );
    }

    #[test]
    fn flood_retains_targeted_question_with_bounded_fair_queue() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        for index in 0..512 {
            inbox.push(
                &message(
                    &format!("n{index}"),
                    &format!("u{index}"),
                    DanmuEventKind::Danmu,
                    "普通内容",
                ),
                now,
            );
        }
        let mut question = message(
            "question",
            "questioner",
            DanmuEventKind::Danmu,
            "@主播 能解释吗",
        );
        question.event.reply_to = Some("主播".into());
        inbox.push(&question, now);
        for index in 0..8 {
            inbox.push(
                &message(
                    &format!("p{index}"),
                    &format!("p{index}"),
                    DanmuEventKind::Danmu,
                    "@主播 重要",
                ),
                now,
            );
        }
        let models = inbox.models(now);
        assert_eq!(models.len(), 8);
        assert_eq!(
            models
                .iter()
                .filter(|m| m.event.content.contains("@主播"))
                .count(),
            6
        );
        assert_eq!(
            models
                .iter()
                .filter(|m| !m.event.content.contains("@主播"))
                .count(),
            2
        );
        assert!(inbox.get("question").is_some());
        assert_eq!(inbox.snapshot()["pending_models"], 128);
    }

    #[test]
    fn serialized_queue_is_bounded_by_count_and_bytes() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        for index in 0..520 {
            inbox.push(
                &message(
                    &format!("large-{index}"),
                    &format!("author-{index}"),
                    DanmuEventKind::Danmu,
                    &format!("{index}-{}", "x".repeat(8 * 1024)),
                ),
                now,
            );
        }
        let messages = inbox
            .models
            .iter()
            .map(|queued| &queued.message)
            .collect::<Vec<_>>();
        assert!(messages.len() <= MODEL_CAPACITY);
        assert!(serde_json::to_vec(&messages).unwrap().len() <= MODEL_BYTES);
    }

    #[test]
    fn dedup_is_per_author_and_expires_without_refresh() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        inbox.push(&message("a", "one", DanmuEventKind::Danmu, " same "), now);
        inbox.push(&message("b", "two", DanmuEventKind::Danmu, "same"), now);
        inbox.push(
            &message("c", "one", DanmuEventKind::Danmu, "same"),
            now + Duration::from_secs(10),
        );
        inbox.push(
            &message("d", "one", DanmuEventKind::Danmu, "same"),
            now + Duration::from_secs(31),
        );
        assert_eq!(inbox.snapshot()["pending_models"], 3);
        assert_eq!(inbox.snapshot()["deduplicated"], 1);
    }

    #[test]
    fn templates_coalesce_without_interpolating_body_and_obey_gaps() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        inbox.push(
            &message("g1", "one", DanmuEventKind::Gift, "恶意正文 999份"),
            now,
        );
        inbox.push(
            &message("g2", "two", DanmuEventKind::Gift, "另一段正文"),
            now + Duration::from_secs(1),
        );
        assert!(inbox.template(now + Duration::from_secs(4)).is_none());
        let gift = inbox.template(now + Duration::from_secs(5)).unwrap();
        assert!(!gift.text.contains("恶意正文") && !gift.text.contains("999"));
        assert!(gift.mention_sender);
        inbox.consume_template(&gift.message.event.id, now + Duration::from_secs(5));
        inbox.push(
            &message("f", "three", DanmuEventKind::Follow, "关注"),
            now + Duration::from_secs(6),
        );
        assert!(inbox.template(now + Duration::from_secs(19)).is_none());
        let second = inbox.template(now + Duration::from_secs(20)).unwrap();
        assert_eq!(second.message.event.author_id.as_deref(), Some("two"));
        assert!(second.mention_sender);
        assert_ne!(gift.text, second.text);
        inbox.consume_template(&second.message.event.id, now + Duration::from_secs(20));
        assert_eq!(
            inbox
                .template(now + Duration::from_secs(35))
                .unwrap()
                .message
                .event
                .id,
            "f"
        );
        assert_eq!(inbox.snapshot()["merged"], 1);
    }

    #[test]
    fn peeking_does_not_refresh_expiry_and_oversized_does_not_pause() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        inbox.push(
            &message("old", "one", DanmuEventKind::Danmu, "still useful"),
            now,
        );
        assert_eq!(inbox.models(now + Duration::from_secs(119)).len(), 1);
        assert!(inbox.models(now + MAX_AGE).is_empty());

        inbox.push(
            &message(
                "huge",
                "two",
                DanmuEventKind::Danmu,
                &"x".repeat(MODEL_BYTES + 1),
            ),
            now + MAX_AGE,
        );
        inbox.push(
            &message("next", "three", DanmuEventKind::Danmu, "accepted"),
            now + MAX_AGE,
        );
        assert_eq!(inbox.models(now + MAX_AGE).len(), 1);
        assert_eq!(inbox.snapshot()["overflow"], 1);
    }
    #[test]
    fn only_more_than_five_distinct_viewers_use_collective_thanks() {
        let now = Instant::now();
        let mut small = Inbox::new(policy());
        for i in 0..5 {
            small.push(
                &message(
                    &format!("g{i}"),
                    &i.to_string(),
                    DanmuEventKind::Gift,
                    "礼物",
                ),
                now,
            );
        }
        let mut large = Inbox::new(policy());
        for i in 0..6 {
            large.push(
                &message(
                    &format!("g{i}"),
                    &i.to_string(),
                    DanmuEventKind::Gift,
                    "礼物",
                ),
                now,
            );
        }
        let collective = large.template(now + TEMPLATE_BATCH).unwrap();
        assert!(!collective.mention_sender);
        large.consume_template(&collective.message.event.id, now + TEMPLATE_BATCH);
        assert!(large.template(now + Duration::from_secs(20)).is_none());
        for i in 0..5 {
            let at = now + Duration::from_secs(5 + i * 15);
            let personal = small.template(at).unwrap();
            assert!(personal.mention_sender);
            assert_eq!(
                personal.message.event.author_id.as_deref(),
                Some(i.to_string().as_str())
            );
            assert_ne!(personal.text, collective.text);
            small.consume_template(&personal.message.event.id, at);
        }
        assert!(small.template(now + Duration::from_secs(80)).is_none());
    }

    #[test]
    fn repeated_actions_from_one_viewer_do_not_turn_into_a_crowd() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        for i in 0..20 {
            inbox.push(
                &message(
                    &format!("g{i}"),
                    "17",
                    DanmuEventKind::Gift,
                    &format!("礼物{i}"),
                ),
                now,
            );
        }
        let one = inbox.template(now + TEMPLATE_BATCH).unwrap();
        assert!(one.mention_sender);
        inbox.consume_template(&one.message.event.id, now + TEMPLATE_BATCH);
        assert!(inbox.template(now + Duration::from_secs(20)).is_none());
    }

    #[test]
    fn share_batches_freeze_recipients_and_rotate_only_after_consumption() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        for i in 0..4 {
            inbox.push(
                &message(
                    &format!("s{i}"),
                    &i.to_string(),
                    DanmuEventKind::Share,
                    "分享",
                ),
                now,
            );
        }
        let first = inbox.template(now + TEMPLATE_BATCH).unwrap();
        assert_eq!(
            inbox.template(now + Duration::from_secs(6)).unwrap().text,
            first.text
        );
        for i in 4..10 {
            inbox.push(
                &message(
                    &format!("s{i}"),
                    &i.to_string(),
                    DanmuEventKind::Share,
                    "分享",
                ),
                now + Duration::from_secs(6),
            );
        }
        let mut variants = std::collections::HashSet::new();
        for i in 0..4 {
            let at = now + Duration::from_secs(6 + i * 15);
            let next = inbox.template(at).unwrap();
            assert!(next.mention_sender);
            assert_eq!(next.message.event.id, format!("s{i}"));
            if i < 3 {
                assert!(variants.insert(next.text.clone()));
            } else {
                assert_eq!(next.text, first.text);
            }
            inbox.consume_template(&next.message.event.id, at);
        }
        assert!(inbox.template(now + Duration::from_secs(110)).is_none());
        let next_batch = inbox.template(now + Duration::from_secs(111)).unwrap();
        assert!(!next_batch.mention_sender);
        assert_eq!(next_batch.message.event.id, "s4");
    }

    #[test]
    fn paused_expired_or_disabled_batches_never_resume_the_remaining_recipients() {
        let now = Instant::now();
        let mut inbox = Inbox::new(policy());
        for i in 0..2 {
            inbox.push(
                &message(
                    &i.to_string(),
                    &i.to_string(),
                    DanmuEventKind::Follow,
                    "关注",
                ),
                now,
            );
        }
        let first = inbox.template(now + TEMPLATE_BATCH).unwrap();
        inbox.consume_template(&first.message.event.id, now + TEMPLATE_BATCH);
        inbox.clear();
        assert!(inbox.template(now + Duration::from_secs(20)).is_none());
        inbox.push(&message("exp", "2", DanmuEventKind::Gift, "礼物"), now);
        assert!(inbox.template(now + TEMPLATE_BATCH).is_some());
        assert!(inbox.template(now + MAX_AGE).is_none());
        inbox.push(
            &message("off", "3", DanmuEventKind::Share, "分享"),
            now + MAX_AGE,
        );
        assert!(inbox.template(now + MAX_AGE + TEMPLATE_BATCH).is_some());
        let mut disabled = policy();
        disabled.thank_shares = false;
        inbox.configure(disabled);
        assert!(
            inbox
                .template(now + MAX_AGE + Duration::from_secs(20))
                .is_none()
        );
    }
}
