# Step 2：删除插件系统、Trace 录制与本地 commit_message

## 结论

插件系统（Deno 运行时 + 3 套内置认证插件 + 前端插件页）、Cursor 二进制 Trace 录制、
以及本地 `WriteGitCommitMessage` 生成引擎已全部移除，主链路（data_binary 解码、
RunInference 拒绝回退、BootstrapStatsig 门控、14 模型目录注入、工具调用、上下文压缩、
首字性能）行为不变。

- `cargo test --workspace --all-targets`：**全绿**（见 §3，逐二进制列出）
- `make check`：**通过**（fmt + clippy `-D warnings` + cargo test + 前端 typecheck/build）
- 删除量：`git diff --stat 0bbb47e..HEAD | tail -1` → **122 files changed, 639 insertions(+), 20148 deletions(-)**
- 依赖：`cursor-server` 的 `zip`（Deno 运行时解压唯一使用者）已移除；`cargo tree -p cursor-server` 中无任何插件/JS 运行时相关 crate
- 产物：arm64、50 MB、adhoc 签名验证通过

---

## 1. 删除的目录与文件

| 路径 | 说明 |
|---|---|
| `server/src/plugin/`（17 文件 / 5,839 行） | 插件发现、Deno 运行时下载与校验、JS worker、OAuth 回调 server、SDK |
| `server/plugins/build-in/`（33 文件） | codex-auth / antigravity-auth / grok-auth 三套 Deno 插件 |
| `server/src/cursor/services/observability/`（3 文件 / 577 行） | 异步 trace 队列与落库 worker |
| `server/src/store/cursor_traces.rs`（345 行） | `cursor_run_traces` / 附件持久化 |
| `server/src/cursor/services/commit_message.rs`（403 行） | 本地 commit message 生成引擎 |
| `server/prompt/cursor/commit/` | 两套 commit 默认提示词 |
| `server/src/control/plugins.rs`（160 行） | `/__byok-api__/api/plugins/**` 15 条路由 |
| `apps/desktop/src/features/plugins/`（4 文件 / 1,255 行） | 插件管理页与资源面板 |
| `apps/desktop/src/features/settings/CommitSettingsCard.tsx` + scss（231 行） | Commit 模型/提示词设置卡 |
| `server/tests/cursor_trace_queue.rs`（129 行） | trace 队列测试 |
| `server/tests/commit_message.rs`（251 行） | commit 引擎端到端测试 |

## 2. 修改的代码（按目录）

### server/src

| 文件 | 改动 |
|---|---|
| `app.rs` | 删除 `PluginRuntime` / `PluginRegistry` 初始化（不再扫描 `~/.cursor-byok-v3/plugins`），改用 `TransportRegistry::with_web_cache`、`ControlService::new(store, provider, clients)` |
| `config.rs` | 删除只为插件 `minAppVersion` 门控存在的 `Config::app_version` |
| `provider/router.rs` | 删除 `PluginModelProvider` 分支与 `ADAPTER_ID_PREFIX`，`stream()` 只剩一条内置模型装配路径 |
| `provider/recorder.rs` | 不再写 `llm_call_requests` / `llm_call_response_chunks`；请求只记 `request_bytes`，响应按窗口累计 `response_bytes` / `stream_event_count` / `first_event_at_ms`；`llm_calls` 主表的 ttfb/ttft/ttfr/duration/token 用量照常写入 |
| `provider/{anthropic,openai_chat,openai_responses}.rs` | 去掉 `recorded_headers`（仅明细表使用） |
| `store/llm_calls.rs` | 删除 `detailed_logging` / `set_detailed_logging`、请求/分块明细读写；新增 `record_llm_stream_totals` |
| `store/settings.rs` | 删除 `CommitSettings`、`CommitPromptLocale`、`commit_settings` / `set_commit_settings` |
| `store/storage.rs` | 统计只统计 `llm_calls`；清理不再触碰 trace 表 |
| `store/mod.rs` | 删除 `cursor_traces` 模块与 `BufferedCursorTraceChunk` |
| `model/configuration.rs` | 删除 `ProviderType::Plugin` 与 `is_sensitive_header` |
| `model/observability.rs` | 删除 `CursorRunTraceSummary` / `CursorRunTraceArtifact` / `LlmCallRequest` / `LlmCallResponseChunk`，`NewLlmCall`、`LlmCallSummary` 去掉 `detailed` |
| `cursor/transport/{handle,registry}.rs` | 删除 `CursorTraceRecorder` 字段、`trace()`、`CursorTraceService`、`with_plugins`、`plugins()` |
| `cursor/services/blob_sync.rs` | 删除 5 处 trace 埋点与 `trace()` 访问器 |
| `cursor/checkpoint/builder.rs` | checkpoint 发布不再额外编码一份 trace 工件 |
| `cursor/compile/run.rs` | 删除 history projection trace 埋点 |
| `cursor/services/model_catalog.rs` | `AvailableModels` / `GetUsableModels` / `GetDefaultModel*` 只注入配置模型，删除 `available_plugin_model` / `usable_plugin_model` |
| `cursor/services/mod.rs` | 删除 `commit_message`、`observability` 模块 |
| `api/cursor/handlers.rs` | 删除 `WriteGitCommitMessage` 路由；`bidi_handler` 只按 `model_configs` 判定本地路由，删除全部 trace 调用 |
| `api/cursor/run_sse.rs` | 删除 `TraceStreamSink` 与 trace 参数 |
| `api/cursor/bidi.rs` | 删除仅服务 trace 的 `trace_metadata()` / `client_message_type()` / `history_image_count()` |
| `control/mod.rs` | 删除插件 15 条路由、`/settings/observability`、`/settings/commit` |
| `control/service.rs` | 删除 plugin_* 方法群、trace 详情、`ObservabilitySettings`、commit 设置、`official_call` |
| `control/settings.rs` | 同上；清理存储不再区分 scope |
| `local_app/proxy.rs` | `is_local_path` 移除 `WriteGitCommitMessage`，让它透明透传上游（新增测试覆盖） |

### 数据库

**未新增、未修改任何迁移文件**（`server/migrations/` 10 个文件零 diff）。
`llm_call_requests`、`llm_call_response_chunks`、`cursor_run_traces`、
`cursor_run_trace_artifacts`、`service_settings.llm_detailed_logging` 等旧表/旧行保留，
留待第 4 步统一处理。

### apps/desktop/src

| 文件 | 改动 |
|---|---|
| `App.tsx` / `shell/AppLayout.tsx` | 删除 `/plugins` 路由、菜单项与 keep-alive 条目 |
| `shared/api.ts` | 删除 `Plugin*` 类型、`pluginText`、`configuredPluginModels`、11 个插件接口、`observability`/`commitSettings`；`CallDetail` 只保留 `call` |
| `shared/store/appStore.ts` | 删除 `plugins` / `pluginRuntime` / `detailed` 状态与 5 个 plugin 动作 |
| `features/models/{CursorModelCards,CursorSettingsPage,CursorGates}.tsx` | 删除插件模型分组、`PluginModelRow`、插件路由跳转 |
| `features/home/HomePage.tsx` | 模型筛选只来自配置模型 |
| `features/settings/SettingsPage.tsx` | 删除“调用观测/详细模式”卡与 commit 卡，清理存储单一口径 |
| `features/calls/{CallDetails,CallTable}.tsx` | 删除请求/响应流/Cursor 追踪 tab 与“详细记录”列 |
| `demo/api.ts` | 同步删除插件、detailed、trace_count 假数据 |
| `i18n/` | `i18n:scan` 重新生成 `catalog.json` 与三套 locale（-1,713 / -110 行），删掉全部插件、commit、详细记录键；新增 4 条清理统计相关译文（en-US / pt-BR） |

## 3. 验收证据

### 3.1 `cargo test --workspace --all-targets`（各二进制通过数）

| 测试二进制 | 通过 | 失败 |
|---|---:|---:|
| `cursor_byok_desktop` (lib) | 3 | 0 |
| `cursor_byok_desktop` (main) | 0 | 0 |
| `cursor_server` (lib) | 129 | 0 |
| `cursor_server` (bin) | 0 | 0 |
| `checkpoint_recovery` | 4 | 0 |
| `compaction` | **6** | 0 |
| `connect_wire` | **10** | 0 |
| `conversation_delivery` | 4 | 0 |
| `cursor_transport_lifecycle` | 2 | 0 |
| `error_lifecycle` | 7 | 0 |
| `interrupt` | 17 | 0 |
| `prefix_stability` | 13 | 0 |
| `tool_round` | **18** | 0 |
| `ttft_probe` | **1** | 0 |

`make check` 退出码 0：`cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo test --workspace --all-targets`、`npm --prefix apps/desktop run check`（typecheck + typecheck:node + vite build）全部通过。

### 3.2 删除量

```
$ git diff --stat 0bbb47e..HEAD | tail -1
 122 files changed, 639 insertions(+), 20148 deletions(-)
```

### 3.3 依赖

移除的 crate：

| crate | 原因 |
|---|---|
| `zip` | 唯一使用者是 `plugin/installation.rs` 的 Deno 运行时解压 |

`Cargo.lock` 未整体重算（其余 lock 条目仍被 `cursor-byok-desktop` 的 `zip` 使用，
Tauri updater 解包依赖它），`server/Cargo.toml` 只删了 `zip` 一行。

```
$ cargo tree -p cursor-server | grep -icE "deno|rusty_v8|oauth"
0
$ cargo tree -i zip --workspace
zip v4.6.1
└── cursor-byok-desktop v1.0.1 (apps/desktop/src-tauri)   # 已不属于 cursor-server
```

### 3.4 arm64 桌面包

```
$ APPLE_SIGNING_IDENTITY="-" make build-desktop
   ... Finished `release` profile [optimized] target(s) in 1m 19s
    Finished 1 bundle at:
        target/release/bundle/macos/Cursor BYOK.app

$ du -sh "$HOME/Applications/cursor-byok-lite-step2/Cursor BYOK.app"
 50M

$ file ".../Cursor BYOK.app/Contents/MacOS/cursor-byok-desktop"
Mach-O 64-bit executable arm64

$ codesign --verify --deep --strict ".../Cursor BYOK.app"
(无输出，退出码 0)

$ codesign -dv ".../Cursor BYOK.app"
Identifier=dev.cursorbyok.desktop
Format=app bundle with Mach-O thin (arm64)
Signature=adhoc
```

## 4. 主链路回归点

| 主链路 | 验证方式 |
|---|---|
| data_binary 解码 | `connect_wire` 10 项（含 `cursor_error_details_subset_decodes_captured_wire_value`） |
| RunInference 拒绝并回退 | `handlers.rs::refuse_run_inference_stream` 未改动；`error_lifecycle` 7 项 |
| BootstrapStatsig 门控覆盖 | `analytics::bootstrap_statsig` 路由与实现未改动 |
| 14 模型注入 | `model_catalog.rs` 仅删插件分支；`AvailableModels` 仍 `models.iter().map(available_model)`；`cli_default_responses_use_the_local_model_hash` 通过 |
| 工具调用 | `tool_round` 18 项全绿 |
| 上下文压缩 | `compaction` 6 项 + `interrupt` 17 项 |
| 首字性能 | `ttft_probe` 1 项通过（13.28s）；`server/src/timing.rs`、迁移 0010 零 diff |

## 5. 未做（留给后续步骤）

- 旧表 `llm_call_requests` / `llm_call_response_chunks` / `cursor_run_traces` /
  `cursor_run_trace_artifacts` 仍在库里，未写删表迁移（第 4 步统一处理）
- TOML 配置、存储层替换、Tauri 前端拆除均未触碰（Step 3+）

## 6. 已知提示（非回归）

`make build-desktop` 输出 `updater secret key does not match pubkey` 警告：
本地 `.tauri/cursor-byok.local.key` 与 `tauri.conf.json` 的 updater pubkey 不匹配，
这是本地构建的既有状态，`0bbb47e` 之前即如此，与本次改动无关。
