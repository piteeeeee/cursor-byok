# Cursor BYOK 首字慢（TTFT）根因与修复

## 结论

助手在调用模型前空等的根因**不是** 11 GB 库的写入、blob 或事务，而是**两条未走索引的 SQL**：

| # | 位置 | 问题 | 修复前 | 修复后 |
|---|---|---|---|---|
| 1 | `server/src/store/messages.rs:77` `load_checkpoint_messages_tx` | 递归 CTE `lineage` + `JOIN checkpoint_messages` 被 SQLite 排成**全表扫描 `checkpoint_messages`（124 万行）**，再用 bloom filter 过滤 | 5.1 s（冷）/ 1.9 s（热） | 1–17 ms |
| 2 | `server/src/store/llm_calls.rs:275` `latest_context_usage` | `WHERE conversation_id = ? AND json_extract(...) IS NOT NULL ORDER BY created_at_ms DESC` 无可用索引，**全表扫描 `llm_calls`（68,261 行）+ 临时 B 树排序** | 1.5–3.9 s | < 1 ms |

两处都在每次 Run 的**首字前必经路径**上，且每轮对话会重复调用 `load_checkpoint_messages` 多次（初始消息、每次 checkpoint、每次压缩判断），所以实际等待是 15–45 s 量级。

同一对话实测（11 GB 库副本，`e17bd116-64a6-46b9-86da-cef9f439356b`）：**17.9 s → 74 ms**。

---

## 1. 测量方法

真实库只读，所有写操作都在 APFS 克隆副本上进行：

```sh
mkdir -p /tmp/ttft-work
cp -c ~/.cursor-byok-v3/cursor-byok.db* /tmp/ttft-work/
```

复现程序 `server/tests/ttft_probe.rs` 用**生产代码本身**跑一遍 Run 前置流程：`TransportRegistry` → `ConversationRuntime` → `compile::prepare` → `RunEngine::run_claimed` → 第一次 `provider.stream(...)`。测试进程扮演 Cursor 一侧的 wire 协议（回 `SetBlobResult`、按 `GetBlobArgs` 供 blob、回 `RequestContextResult`），所以测到的全是助手内部耗时，不含等 Cursor 回传上下文的网络时间。

```sh
TTFT_DATABASE_URL="sqlite:///tmp/ttft-work/after3.db" \
TTFT_CONVERSATION=e17bd116-64a6-46b9-86da-cef9f439356b \
TTFT_ITERATIONS=2 \
cargo test --package cursor-server --test ttft_probe -- --nocapture
```

计时埋点：新增 `server/src/timing.rs`（`Stages`，info 级别逐段输出 `subject/stage/elapsed_ms`），在 `compile::prepare` 与 `RunEngine` 的首字路径上各打点。**修复后保留这些日志。**

## 2. 修复前各段耗时（11 GB 副本，冷克隆）

```
prepare  prefetched blobs                0 ms
prepare  hydrate conversation state      0 ms
prepare  request context                 0 ms
prepare  model configuration             0 ms
prepare  prompt assembly                 0 ms
prepare  base checkpoint import          0 ms
prepare  input anchor                    1 ms
prepare  user event id                   0 ms
prepare  existing runtime message        0 ms
prepare  request context message         0 ms
prepare  initial messages                0 ms
prepare  checkpoint prefix match     5,125 ms   <-- 根因 1
prepare  prepare complete            5,130 ms

run      claim ownership                 2 ms
run      usage anchor                1,495 ms   <-- 根因 2
run      append initial messages     7,892 ms   <-- 根因 1（再次全扫）
run      initial checkpoint published 1,542 ms
run      load checkpoint messages    1,858 ms   <-- 根因 1（再次全扫）
run      project messages                0 ms
run      compaction estimate             0 ms
run      begin provider call             0 ms
run      hydrate tool images             0 ms
run      first provider request     12,791 ms
```

Append → 第一次 provider 请求合计 **17,923 ms**。

单条 SQL 独立复现（冷克隆、只读）：

```sh
time sqlite3 cold_test.db "<load_checkpoint_messages 的 SQL>"
# real 0m5.103s
```

`EXPLAIN QUERY PLAN`（修复前）：

```
|--SCAN rm USING INDEX sqlite_autoindex_checkpoint_messages_1     <-- 全表扫 124 万行
|--SEARCH m USING INDEX sqlite_autoindex_messages_1 (...)
|--BLOOM FILTER ON lineage (checkpoint_id=?)
|--SEARCH lineage USING AUTOMATIC COVERING INDEX (checkpoint_id=?)
`--USE TEMP B-TREE FOR ORDER BY
```

`rm` 被选为驱动表，`lineage` 反过来被 bloom filter 过滤——递归 CTE 只有 8 行，却去扫 124 万行的表。库越大越慢，这正是「11 GB 库导致首字慢」的真实机制：**不是读写慢，是缺合适的连接顺序。**

`llm_calls` 也有同样问题：

```
|--SCAN llm_calls
`--USE TEMP B-TREE FOR ORDER BY
```

`llm_calls` 现有索引只有 `created_at_ms`、`(run_id, provider_call_index)`、`(model_hash, created_at_ms)`，没有以 `conversation_id` 开头的索引。

### 关于 context_sync 的 12 秒

`requesting uncached Cursor context` → `Run claimed conversation ownership` 之间 12.6 s **不是助手在算**，主要是在等 Cursor 回传：

- 日志同一区间有独立的 `GetEffectiveUserPlugins` 转发记录（耗时 300–350 ms），说明这期间 Cursor 还在做自己的初始化；
- 该等待发生在 `context_sync::load` 的 `tokio::select!` 上（`receiver` / `cancellation` / 60 s 超时），是纯等待，本地无计算；
- 修复前后这段等待都在（它不计入本次目标的助手内部耗时）。

本次目标「routing → 第一次发出 LLM 请求的助手内部耗时」把这段排除在外。**排除后**的助手内部耗时：修复前 17.9 s，修复后 74 ms。

## 3. 修复

只改根因所在的两处，不加兼容层、不动其他结构。

### 3.1 `server/src/store/messages.rs` — 固定连接顺序

```sql
 SELECT m.payload_json
 FROM lineage
-JOIN checkpoint_messages rm ON rm.checkpoint_id = lineage.checkpoint_id
-JOIN messages m
+CROSS JOIN checkpoint_messages rm
+  ON rm.checkpoint_id = lineage.checkpoint_id
+CROSS JOIN messages m
   ON m.conversation_id = rm.conversation_id AND m.message_id = rm.message_id
 ORDER BY lineage.depth DESC, rm.ordinal ASC
```

`CROSS JOIN` 在 SQLite 里是「**禁止重排**」的语义糖，强制 `lineage`（8 行）当驱动表。没有新增索引，没有改表结构，结果集逐字节一致（同一对话 8 行 payload 的 md5 相同）。

修复后 `EXPLAIN QUERY PLAN`：

```
|--SCAN lineage
|--SEARCH rm USING INDEX sqlite_autoindex_checkpoint_messages_1 (checkpoint_id=?)
|--SEARCH m USING INDEX sqlite_autoindex_messages_1 (conversation_id=? AND message_id=?)
`--USE TEMP B-TREE FOR ORDER BY
```

### 3.2 `server/migrations/0010_index_llm_calls_conversation.sql` — 补索引

```sql
CREATE INDEX llm_calls_conversation ON llm_calls(conversation_id, created_at_ms DESC);
```

修复后计划：

```
`--SEARCH llm_calls USING INDEX llm_calls_conversation (conversation_id=?)
```

与 `usage_json` 的 `json_extract` 过滤无关——先按对话定位、再按时间倒序取第一条，正是这个索引的顺序。

**在 11 GB 副本上实测建索引耗时：**

| 方式 | 耗时 |
|---|---|
| 手工 `CREATE INDEX`（`sqlite3`，11.5 GB 副本） | 4.4 s |
| 走真实 sqlx 迁移流程（`Store::connect` 内，含前后校验） | 2.4 s |

`llm_calls` 只有 68,261 行 / 66 MB，是库里最小的大表，建索引是 O(小表) 而非 O(11 GB)。

## 4. 修复后各段耗时（同一 11 GB 副本，冷克隆，含迁移执行）

```
prepare  checkpoint prefix match        17 ms   (was 5,125)
prepare  prepare complete               22 ms   (was 5,130)

run      usage anchor                    0 ms   (was 1,495)
run      append initial messages        20 ms   (was 7,892)
run      initial checkpoint published   25 ms   (was 1,542)
run      load checkpoint messages        1 ms   (was 1,858)
run      first provider request         50 ms   (was 12,791)
```

Append → 第一次 provider 请求：

| 轮次 | 修复前 | 修复后 |
|---|---|---|
| 1 | 17,923 ms | **74 ms** |
| 2 | — | 65 ms |
| 3 | — | 119 ms |

目标 < 2 s 达成，余量约 25 倍。

## 5. 回归防线

`server/tests/ttft_probe.rs::run_prefix_does_not_scan_unrelated_checkpoint_history` 不依赖真实库：造 100 万条无关对话的 `checkpoint_messages` 引用（真实库 124 万条），用合成 RunRequest 跑通生产路径，断言首字前 < 1 s。

已验证它会真的失败：

```
# 把 CROSS JOIN 改回 JOIN 后
first provider request took 7041 ms; the Run prefix must not scan checkpoint history
# 修复后
iteration 0: append -> first provider request 9 ms
iteration 1: append -> first provider request 8 ms
iteration 2: append -> first provider request 19 ms
```

## 6. 测试

`cargo test --package cursor-server`：**246 passed, 0 failed**

| 测试二进制 | 通过 |
|---|---|
| lib（`src/lib.rs`） | 157 |
| bin（`src/bin/cursor-server.rs`） | 0 |
| checkpoint_recovery | 4 |
| commit_message | 5 |
| compaction | 6 |
| connect_wire | 10 |
| conversation_delivery | 4 |
| cursor_trace_queue | 2 |
| cursor_transport_lifecycle | 2 |
| error_lifecycle | 7 |
| interrupt | 17 |
| prefix_stability | 13 |
| tool_round | 18 |
| ttft_probe | 1 |

## 7. 未做的事

- 没有做轻量化方案（`.scratch/byok-lite/plan.md`）里的内存化改造。
- 没有删数据，没有对真实库跑迁移。
- `latest_context_usage` 的 `ORDER BY created_at_ms DESC, rowid DESC` 里 `rowid` 那一项仍会触发一次小范围排序（同对话 4 行），实测 < 1 ms，没有再优化。
