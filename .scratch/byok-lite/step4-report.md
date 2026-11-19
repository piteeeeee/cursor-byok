# Step 4：会话转内存，用量独立落盘

以 `7a18863` 为基线。首次新会话实测 5 ms，第二个新会话命中同一组上下文引用，实测 3 ms（从提交 Run 到 provider 首次收到请求，不含模型网络 RTT）。临时目录中经过 20 次本地模拟上游调用、WAL 刷回后，`usage.db` 实测 **20,480 字节**。旧历史与统计未迁移。

## 数据路径

```text
Cursor Run（conversation_state + pre_fetched_blobs）
  → MemoryStore：校验 blob，恢复历史，按会话和 Run 检查写入所有权
  → context_sync：四个原始 ID 命中则复用规则等静态部分；动态环境来自本次请求
  → RunEngine → 模型 → 工具 → 本轮结果写入内存 → checkpoint 发布给 Cursor
  → recorder 收到终态 → usage.db → 本地自然日/月汇总、CSV
重启：新 MemoryStore → 回取已发布的消息 → 无锚点估算 → 下次模型用量重建锚点
```

目录树：数字为 `wc -l` 物理行，含空行、内联测试；测试文件单列，未计生成代码。

```text
server/
├── migrations/                    # 删除：旧会话库的 10 份迁移，共 621 行
├── src/store/                     # 内存会话和持久用量各有唯一入口
│   ├── mod.rs                     # 5 行，导出两种存储
│   ├── memory.rs                  # 新增 654 行：256 MiB / 10,000 条 blob LRU，256 组上下文缓存，checkpoint 谱系与 Run/工具状态
│   ├── usage.rs                   # 新增 234 行：单表、按日/月统计、限龄/限量清理、流式 CSV
│   └── [删除]                     # 不再保留 SQLite 会话、设置与旧统计路径（基线物理行）
│       ├── cas.rs (108)、messages.rs (130)、checkpoints.rs (373)、conversations.rs (100)、input_anchors.rs (41)
│       ├── runs.rs (276)、tool_rounds.rs (313)、settings.rs (224)、llm_calls.rs (375)、overview.rs (221)
│       └── sqlite.rs (49)、storage.rs (29)、migrations.rs (490)、writer.rs (15)
├── src/provider/recorder.rs        # 107 行：每个结束的调用只写一条统计；不接收 HTTP body 或响应分块
├── src/cursor/services/context_sync.rs # 190 行：共享四元组缓存，同一运行时保留最近会话上下文
├── src/local_app/state.rs          # 新增 30 行：显式暂停单字段，临时文件 0600 + rename
├── src/control/preferences.rs      # 新增 74 行：桌面展示设置类型；这些非接管偏好本轮仅进程内有效
└── tests/                         # 运行时验证，不依赖真实 Cursor 进程
    ├── checkpoint_recovery.rs     # 613 行：冷启动、工具历史、锚点、KV 校验
    ├── ttft_probe.rs              # 279 行：冷请求与缓存命中计时、断言只有一次上下文请求
    └── usage_store.rs             # 83 行：本机随机端口 SSE 上游、20 次调用、导出、WAL 刷回验收
apps/desktop/src/features/home/HomePage.tsx # 20 行：今日、本月、导出；删除依赖旧逐调用明细表的视图
```

## 重启与已知风险

- `server/src/cursor/checkpoint/recovery.rs:12` 导入 `pre_fetched_blobs` 并校验 ID；`:36` 按 root ID 调用 `sync.get`。`server/src/cursor/services/blob_sync.rs:121` 先查本地，`:139` 在未命中时发 **KV** `GetBlobArgs`（不是 exec）；`:208` 处理 `GetBlobResult`、校验摘要并缓存。基线已经有该回源路径，本步替换了本地存储。
- 冷启动测试先完成一次 Read 和随后的模型答复，模拟客户端保存真正收到的 SET 数据及最后的 checkpoint；丢弃旧存储再请求，用 GET 重建。检查模型历史含旧问题、工具 ID、结果 `value`、旧答复和新问题。首次无锚点走 `run/compaction.rs:136` 的估算，下一次调用开始前已有上次 provider 的精确锚点。
- 估算按字符和换行而非供应商 tokenizer 计数（`model/token_count.rs:85`），不是上界：中文等输入可能偏低，接近上限的最坏情况是供应商拒绝超窗请求；短行密集的文本也可能偏高而提前压缩。本步没有另加兜底或持久化锚点。
- **多工具批次中途重启**：已完成的 Shell/Edit 可能被重放。恢复边界是已发布 checkpoint，部分结果只保留在服务端（`cursor/conversation/output.rs:559/633/647`、基线 `run/engine.rs:163`，现 `:156`）。这是基线行为，不保证单个外部副作用恰好执行一次。
- **旧 Read 图片**：重启后模型看不到图片字节，只得到该位置的文字占位 `[图片在助手重启后不可用]`；本步不增加图片 KV 回源。单测 `restored_read_image_has_a_model_visible_placeholder` 通过。
- **账户/权益（待决定，本次未改）**：`server/src/local_app/account.rs:41` 及 `:73` 涉及本机 Cursor 账户缓存，`server/src/cursor/services/account.rs:233` 涉及本地权益响应。是否保留由协调者/用户另行决定。

## 验收证据

隔离 HOME 运行；只在本地临时目录创建数据库，未读写运行中的 Step 3 应用目录、未启动或终止 Cursor/桌面进程、未推送。

`cargo test --workspace --all-targets`：223 passed、0 failed。`make check`：退出 0，含 Rust 格式、Clippy `-D warnings`、同一全量测试、前端两组 tsc 和 Vite 构建（`✓ built in 603ms`）。二进制通过数如下：

| 测试目标 | 通过数 |
|---|---:|
| desktop lib / main | 3 / 0 |
| server lib / byok-export / cursor-server bin | 138 / 0 / 0 |
| checkpoint_recovery / compaction / connect_wire | 3 / 6 / 10 |
| conversation_delivery / cursor_transport_lifecycle / error_lifecycle | 4 / 2 / 7 |
| interrupt / prefix_stability / tool_round | 17 / 13 / 18 |
| ttft_probe / usage_store | 1 / 1 |

```text
test cold_restart_recovers_completed_tool_history_and_rebuilds_usage_anchor ... ok
test local_app::pause_tests::explicit_pause_survives_restart_even_when_ca_and_backend_are_ready ... ok
iteration 0: append -> first provider request 5 ms
iteration 1: append -> first provider request 3 ms
```

`rg -n 'cursor-byok-v3|cursor-byok.db' server/src` 输出全部位于首次初始化：

```text
server/src/config/paths.rs:40:            previous_database: home.join(".cursor-byok-v3").join("cursor-byok.db"),
server/src/config/paths.rs:41:            previous_ca_dir: home.join(".cursor-byok-v3").join(CA_DIR_NAME),
server/src/config/paths.rs:68:/// The previous build kept models, ports, and the CA in `~/.cursor-byok-v3`.
```

`cargo test -p cursor-server --test usage_store -- --nocapture`：20 次本机 SSE 调用，导出 20 行；执行 `PRAGMA wal_checkpoint(TRUNCATE)` 并断言无 busy、表内仍有 20 行，再测量主文件：

```csv
ts_iso,model_id,display_name,provider_name,input_tokens,output_tokens,cache_read_tokens,reasoning_tokens,total_tokens,ttft_ms,duration_ms,status,error_summary
2026-11-19T16:37:29.408Z,model,test,mock,8,2,0,0,10,20,20,ok,
2026-11-19T16:37:29.410Z,model,test,mock,8,2,0,0,10,0,0,ok,
```

`git diff --stat 7a18863..HEAD | tail -1`：`113 files changed, 1960 insertions(+), 6905 deletions(-)`。

**桌面 arm64 包、签名、复制：按协调者指示本轮跳过，由协调者单独构建。** 无本轮包路径、`du`、`file` 或 `codesign` 结果。
