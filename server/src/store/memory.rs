//! Process-local conversation checkpoints, run ownership, and Cursor caches.
use crate::{cursor::protocol::proto::agent::v1 as pb, model::*, Error, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use lru::LruCache;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, num::NonZeroUsize, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlobId([u8; 32]);
impl BlobId {
    pub fn digest(data: &[u8]) -> Self {
        Self(Sha256::digest(data).into())
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self(bytes.try_into().map_err(|_| {
            Error::Protocol(format!("BlobID must be 32 bytes, got {}", bytes.len()))
        })?))
    }
    pub fn from_base64(value: &str) -> Result<Self> {
        Self::from_bytes(
            &STANDARD
                .decode(value)
                .map_err(|e| Error::Protocol(format!("invalid BlobID base64: {e}")))?,
        )
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }
}
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
type ContextKey = [Option<BlobId>; 4];

#[derive(Clone)]
pub struct MemoryStore {
    blobs: Arc<Mutex<Blobs>>,
    contexts: Arc<Mutex<LruCache<ContextKey, Arc<pb::RequestContext>>>>,
    sessions: Arc<Mutex<Sessions>>,
}
struct Blobs {
    entries: LruCache<BlobId, Vec<u8>>,
    bytes: usize,
    limit: usize,
}
#[derive(Default)]
struct Sessions {
    next: i64,
    conversations: HashMap<ConversationId, Session>,
    checkpoints: HashMap<CheckpointId, Node>,
    runs: HashMap<RunId, Run>,
    rounds: HashMap<ToolRoundId, ToolRoundSnapshot>,
    idle: Vec<ConversationId>,
}
struct Session {
    snapshot: Conversation,
    root: CheckpointId,
    anchors: HashMap<String, CheckpointId>,
    messages: HashMap<String, Arc<CanonicalMessage>>,
    children: Vec<CheckpointId>,
    usage: Option<ContextUsageAnchor>,
}
struct Node {
    owner: ConversationId,
    parent: Option<CheckpointId>,
    delta: Vec<Arc<CanonicalMessage>>,
}
struct Run {
    owner: ConversationId,
    request: Option<String>,
    index: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextUsageAnchor {
    pub context_input_tokens: u64,
    pub message_count: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimedRun {
    pub run_id: RunId,
    pub conversation_id: ConversationId,
    pub head_checkpoint_id: CheckpointId,
    pub replaced_run_id: Option<RunId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRoundStatus {
    Pending,
    Settled,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ToolRoundSnapshot {
    pub round_id: ToolRoundId,
    pub run_id: RunId,
    pub base_checkpoint_id: CheckpointId,
    pub assistant: ToolRoundAssistant,
    pub calls: Vec<ToolCall>,
    pub completed_call_ids: Vec<String>,
    pub status: ToolRoundStatus,
    pub version: u64,
    pub created_at_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCommit {
    pub checkpoint_id: CheckpointId,
    pub tool_round_version: u64,
    pub completion_seq: u64,
    pub settled: bool,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::with_limits(10_000, 256 * 1024 * 1024)
    }
}
impl MemoryStore {
    fn with_limits(count: usize, limit: usize) -> Self {
        Self {
            blobs: Arc::new(Mutex::new(Blobs {
                entries: LruCache::new(NonZeroUsize::new(count).unwrap()),
                bytes: 0,
                limit,
            })),
            contexts: Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(256).unwrap()))),
            sessions: Arc::default(),
        }
    }
    pub fn put_blob(&self, data: &[u8]) -> BlobId {
        let id = BlobId::digest(data);
        let mut c = self.blobs.lock();
        if data.len() > c.limit || c.entries.get(&id).is_some() {
            return id;
        }
        while c.bytes + data.len() > c.limit {
            let (_, old) = c.entries.pop_lru().unwrap();
            c.bytes -= old.len();
        }
        if let Some((_, old)) = c.entries.push(id.clone(), data.to_vec()) {
            c.bytes -= old.len();
        }
        c.bytes += data.len();
        id
    }
    pub fn get_blob(&self, id: &BlobId) -> Option<Vec<u8>> {
        self.blobs.lock().entries.get(id).cloned()
    }
    pub(crate) fn context(&self, key: &ContextKey) -> Option<Arc<pb::RequestContext>> {
        self.contexts.lock().get(key).cloned()
    }
    pub(crate) fn cache_context(&self, key: ContextKey, parts: pb::RequestContext) {
        self.contexts.lock().put(key, Arc::new(parts));
    }
    pub fn latest_context_usage(&self, id: &ConversationId) -> Option<ContextUsageAnchor> {
        self.sessions.lock().conversations.get(id)?.usage
    }
    pub(crate) fn set_usage(
        &self,
        c: &ConversationId,
        r: &RunId,
        anchor: Option<ContextUsageAnchor>,
    ) {
        if let Some(session) = self.sessions.lock().conversations.get_mut(c) {
            if session.snapshot.active_run_id.as_ref() == Some(r) {
                session.usage = anchor;
            }
        }
    }
    pub async fn ensure_conversation(&self, c: &ConversationId) -> Result<CheckpointId> {
        Ok(self.sessions.lock().ensure(c))
    }
    pub async fn conversation(&self, c: &ConversationId) -> Result<Option<Conversation>> {
        Ok(self
            .sessions
            .lock()
            .conversations
            .get(c)
            .map(|s| s.snapshot.clone()))
    }
    pub async fn load_checkpoint_messages(
        &self,
        id: CheckpointId,
    ) -> Result<Vec<CanonicalMessage>> {
        self.sessions.lock().history(id)
    }
    pub async fn checkpoint_parent(&self, id: CheckpointId) -> Result<Option<CheckpointId>> {
        Ok(self
            .sessions
            .lock()
            .checkpoints
            .get(&id)
            .and_then(|n| n.parent))
    }
    pub async fn load_current_messages(&self, c: &ConversationId) -> Result<Vec<CanonicalMessage>> {
        let s = self.sessions.lock();
        match s.conversations.get(c) {
            Some(v) => s.history(v.snapshot.current_checkpoint_id),
            None => Ok(vec![]),
        }
    }
    pub async fn message(&self, c: &ConversationId, id: &str) -> Result<Option<CanonicalMessage>> {
        Ok(self
            .sessions
            .lock()
            .conversations
            .get(c)
            .and_then(|v| v.messages.get(id))
            .map(|m| m.as_ref().clone()))
    }
    pub async fn anchor_input(
        &self,
        c: &ConversationId,
        input: &str,
        base: CheckpointId,
    ) -> Result<CheckpointId> {
        let mut s = self.sessions.lock();
        s.ensure(c);
        Ok(*s
            .conversations
            .get_mut(c)
            .unwrap()
            .anchors
            .entry(input.into())
            .or_insert(base))
    }
    pub async fn match_checkpoint_prefix(
        &self,
        c: &ConversationId,
        mut base: CheckpointId,
        add: &[CanonicalMessage],
    ) -> Result<(CheckpointId, usize)> {
        let s = self.sessions.lock();
        for (i, m) in add.iter().enumerate() {
            let Some(child) = s.conversations[c].children.iter().find(|id| {
                let n = &s.checkpoints[id];
                n.parent == Some(base) && n.delta.len() == 1 && n.delta[0].as_ref() == m
            }) else {
                return Ok((base, i));
            };
            base = *child;
        }
        Ok((base, add.len()))
    }
    pub async fn import_checkpoint(
        &self,
        c: &ConversationId,
        msgs: &[CanonicalMessage],
    ) -> Result<CheckpointId> {
        let mut s = self.sessions.lock();
        let head = s.ensure(c);
        let current = s.history(head)?;
        if current == msgs {
            return Ok(head);
        }
        for id in &s.conversations[c].children {
            if *id != head && s.history(*id)? == msgs {
                return Ok(*id);
            }
        }
        let (parent, add) = if msgs.starts_with(&current) {
            (head, &msgs[current.len()..])
        } else {
            (s.conversations[c].root, msgs)
        };
        s.node(c, parent, add)
    }
    pub async fn append_checkpoint(
        &self,
        c: &ConversationId,
        r: &RunId,
        base: CheckpointId,
        add: &[CanonicalMessage],
    ) -> Result<CheckpointId> {
        self.sessions.lock().advance(c, r, base, add, false)
    }
    pub async fn replace_checkpoint(
        &self,
        c: &ConversationId,
        r: &RunId,
        base: CheckpointId,
        add: &[CanonicalMessage],
    ) -> Result<CheckpointId> {
        self.sessions.lock().advance(c, r, base, add, true)
    }
    pub async fn append_message_once(
        &self,
        c: &ConversationId,
        r: &RunId,
        base: CheckpointId,
        m: &CanonicalMessage,
    ) -> Result<(CheckpointId, bool)> {
        let mut s = self.sessions.lock();
        s.owned(c, r, base)?;
        if let Some(old) = s.history(base)?.iter().find(|old| {
            old.message_id == m.message_id
                || m.runtime_event_id.is_some() && old.runtime_event_id == m.runtime_event_id
        }) {
            return if old == m {
                Ok((base, false))
            } else {
                Err(Error::Store(
                    "message id or runtime event reused with different content".into(),
                ))
            };
        }
        Ok((s.advance(c, r, base, std::slice::from_ref(m), false)?, true))
    }
    pub async fn claim_run(&self, p: &PreparedRun) -> Result<ClaimedRun> {
        let mut s = self.sessions.lock();
        let c = &p.conversation_id;
        let base = p.base_checkpoint_id;
        if s.checkpoints.get(&base).is_none_or(|n| n.owner != *c) {
            return Err(Error::Store(
                "base checkpoint does not belong to conversation".into(),
            ));
        }
        s.idle.retain(|x| x != c);
        let session = s.conversations.get_mut(c).unwrap();
        session.snapshot.current_checkpoint_id = base;
        let replaced = session.snapshot.active_run_id.replace(p.run_id.clone());
        if let Some(old) = &replaced {
            s.runs.remove(old);
            s.rounds.retain(|_, v| &v.run_id != old);
        }
        s.runs.insert(
            p.run_id.clone(),
            Run {
                owner: c.clone(),
                request: p.cursor_request_id.clone(),
                index: 0,
            },
        );
        Ok(ClaimedRun {
            run_id: p.run_id.clone(),
            conversation_id: c.clone(),
            head_checkpoint_id: base,
            replaced_run_id: replaced,
        })
    }
    pub async fn active_run_for_cursor_request(&self, id: &str) -> Result<Option<RunId>> {
        Ok(self
            .sessions
            .lock()
            .runs
            .iter()
            .find(|(_, r)| r.request.as_deref() == Some(id))
            .map(|(id, _)| id.clone()))
    }
    pub async fn begin_provider_call(&self, r: &RunId) -> Result<u64> {
        let mut s = self.sessions.lock();
        let run = s
            .runs
            .get_mut(r)
            .ok_or_else(|| Error::Store(format!("run is not active: {r}")))?;
        let index = run.index;
        run.index += 1;
        Ok(index)
    }
    pub async fn finish_run(
        &self,
        r: &RunId,
        _status: RunStatus,
        _usage: Option<Usage>,
        _failure: Option<(&str, &str)>,
    ) -> Result<bool> {
        let mut s = self.sessions.lock();
        let Some(run) = s.runs.remove(r) else {
            return Ok(false);
        };
        s.rounds.retain(|_, v| &v.run_id != r);
        let c = s.conversations.get_mut(&run.owner).unwrap();
        c.snapshot.active_run_id = None;
        s.idle.push(run.owner);
        if s.idle.len() > 128 {
            let old = s.idle.remove(0);
            if let Some(expired) = s.conversations.remove(&old) {
                for id in expired.children {
                    s.checkpoints.remove(&id);
                }
            }
        }
        Ok(true)
    }
    pub async fn create_tool_round(
        &self,
        id: &ToolRoundId,
        r: &RunId,
        base: CheckpointId,
        a: &ToolRoundAssistant,
        calls: &[ToolCall],
        created: Option<u64>,
    ) -> Result<()> {
        let mut s = self.sessions.lock();
        let run = s
            .runs
            .get(r)
            .ok_or_else(|| Error::RunNotFound(r.to_string()))?;
        s.owned(&run.owner, r, base)?;
        if calls.is_empty() || s.rounds.contains_key(id) {
            return Err(Error::Store("empty or duplicate tool round".into()));
        }
        s.rounds.insert(
            id.clone(),
            ToolRoundSnapshot {
                round_id: id.clone(),
                run_id: r.clone(),
                base_checkpoint_id: base,
                assistant: a.clone(),
                calls: calls.to_vec(),
                completed_call_ids: vec![],
                status: ToolRoundStatus::Pending,
                version: 0,
                created_at_ms: created.unwrap_or_else(|| now_ms() as u64),
            },
        );
        Ok(())
    }
    pub async fn tool_round(&self, id: &ToolRoundId) -> Result<Option<ToolRoundSnapshot>> {
        Ok(self.sessions.lock().rounds.get(id).cloned())
    }
    pub async fn commit_tool_result(
        &self,
        c: &ConversationId,
        r: &RunId,
        id: &ToolRoundId,
        result: &ToolResult,
    ) -> Result<ToolCommit> {
        let mut s = self.sessions.lock();
        let round = s
            .rounds
            .get(id)
            .filter(|v| &v.run_id == r)
            .ok_or_else(|| Error::Store("unknown tool round".into()))?;
        if round.completed_call_ids.contains(&result.call_id) {
            return Err(Error::Protocol("duplicate tool result".into()));
        }
        let call = round
            .calls
            .iter()
            .find(|v| v.call_id == result.call_id)
            .ok_or_else(|| Error::Protocol("unknown tool result".into()))?;
        let first = round.version == 0;
        let a = &round.assistant;
        let name = call.name.clone();
        let assistant = CanonicalMessage {
            message_id: format!("{id}:{}:assistant", result.call_id),
            role: Role::Assistant,
            origin: Origin::Assistant,
            content: MessageContent::Assistant {
                text: if first { a.text.clone() } else { String::new() },
                thinking: if first {
                    a.thinking.clone()
                } else {
                    String::new()
                },
                tool_round_id: Some(id.clone()),
                replay_state: first.then(|| a.replay_state.clone()).flatten(),
                tool_calls: vec![ToolCallContent {
                    index: call.index,
                    call_id: call.call_id.clone(),
                    name: name.clone(),
                    arguments: call.arguments.clone(),
                }],
            },
            runtime_event_id: None,
        };
        let tool = CanonicalMessage {
            message_id: format!("{id}:{}:result", result.call_id),
            role: Role::Tool,
            origin: Origin::Tool,
            content: MessageContent::ToolResult(ToolResultContent {
                call_id: result.call_id.clone(),
                name,
                content: result.content.clone(),
                is_error: result.is_error,
                image: result.image.clone(),
                provider_parts: vec![],
            }),
            runtime_event_id: None,
        };
        let head = s.conversations[c].snapshot.current_checkpoint_id;
        let cp = s.advance(c, r, head, &[assistant, tool], false)?;
        let round = s.rounds.get_mut(id).unwrap();
        let seq = round.version;
        round.version += 1;
        round.completed_call_ids.push(result.call_id.clone());
        let settled = round.completed_call_ids.len() == round.calls.len();
        if settled {
            round.status = ToolRoundStatus::Settled;
        }
        Ok(ToolCommit {
            checkpoint_id: cp,
            tool_round_version: round.version,
            completion_seq: seq,
            settled,
        })
    }
}
impl Sessions {
    fn ensure(&mut self, c: &ConversationId) -> CheckpointId {
        if let Some(s) = self.conversations.get(c) {
            return s.snapshot.current_checkpoint_id;
        }
        self.next += 1;
        let id = CheckpointId(self.next);
        self.checkpoints.insert(
            id,
            Node {
                owner: c.clone(),
                parent: None,
                delta: vec![],
            },
        );
        self.conversations.insert(
            c.clone(),
            Session {
                snapshot: Conversation {
                    conversation_id: c.clone(),
                    current_checkpoint_id: id,
                    active_run_id: None,
                },
                root: id,
                anchors: HashMap::new(),
                messages: HashMap::new(),
                children: vec![id],
                usage: None,
            },
        );
        id
    }
    fn history(&self, mut id: CheckpointId) -> Result<Vec<CanonicalMessage>> {
        let mut nodes = vec![];
        loop {
            let n = self
                .checkpoints
                .get(&id)
                .ok_or_else(|| Error::Store(format!("unknown checkpoint {id}")))?;
            nodes.push(n);
            match n.parent {
                Some(p) => id = p,
                None => break,
            }
        }
        Ok(nodes
            .into_iter()
            .rev()
            .flat_map(|n| n.delta.iter().map(|m| m.as_ref().clone()))
            .collect())
    }
    fn owned(&self, c: &ConversationId, r: &RunId, id: CheckpointId) -> Result<()> {
        if self.conversations.get(c).is_some_and(|s| {
            s.snapshot.active_run_id.as_ref() == Some(r) && s.snapshot.current_checkpoint_id == id
        }) {
            Ok(())
        } else {
            Err(Error::Store(format!("lost active ownership for run {r}")))
        }
    }
    fn node(
        &mut self,
        c: &ConversationId,
        parent: CheckpointId,
        add: &[CanonicalMessage],
    ) -> Result<CheckpointId> {
        let s = self.conversations.get_mut(c).unwrap();
        for m in add {
            if s.messages
                .get(&m.message_id)
                .is_some_and(|v| v.as_ref() != m)
            {
                return Err(Error::Store(format!(
                    "conflicting message {}",
                    m.message_id
                )));
            }
        }
        let delta = add
            .iter()
            .map(|m| {
                s.messages
                    .entry(m.message_id.clone())
                    .or_insert_with(|| Arc::new(m.clone()))
                    .clone()
            })
            .collect();
        self.next += 1;
        let id = CheckpointId(self.next);
        self.checkpoints.insert(
            id,
            Node {
                owner: c.clone(),
                parent: Some(parent),
                delta,
            },
        );
        s.children.push(id);
        Ok(id)
    }
    fn advance(
        &mut self,
        c: &ConversationId,
        r: &RunId,
        id: CheckpointId,
        add: &[CanonicalMessage],
        replace: bool,
    ) -> Result<CheckpointId> {
        self.owned(c, r, id)?;
        if add.is_empty() && !replace {
            return Ok(id);
        }
        let parent = if replace {
            self.conversations[c].root
        } else {
            id
        };
        let next = self.node(c, parent, add)?;
        self.conversations
            .get_mut(c)
            .unwrap()
            .snapshot
            .current_checkpoint_id = next;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evicts_by_recency_under_both_limits() {
        let s = MemoryStore::with_limits(2, 5);
        let a = s.put_blob(b"aa");
        let b = s.put_blob(b"bb");
        assert!(s.get_blob(&a).is_some());
        let c = s.put_blob(b"c");
        assert!(s.get_blob(&b).is_none());
        assert!(s.get_blob(&a).is_some());
        let d = s.put_blob(b"dddd");
        assert!(s.get_blob(&a).is_none());
        assert!(s.get_blob(&c).is_none());
        assert!(s.get_blob(&s.put_blob(b"oversize")).is_none());
        assert!(s.get_blob(&d).is_some());
    }
}
