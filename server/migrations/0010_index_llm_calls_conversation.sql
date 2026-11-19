-- Cursor Runs read the newest provider call of a Conversation to anchor the
-- automatic compaction estimate. Without this index that lookup scans every
-- llm_calls row and sorts them, which on a production database costs seconds
-- before the first token of every turn.
CREATE INDEX llm_calls_conversation ON llm_calls(conversation_id, created_at_ms DESC);
