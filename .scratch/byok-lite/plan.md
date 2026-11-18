# cursor-byok 轻量版（byok-lite）实施方案

## 结论先行

1. **减法空间极大**：当前仓库（排除 `target/`、`node_modules/`、二进制字体/图标和 `package-lock.json`）共 **172,324 物理行**（其中生产代码约 **130,300+ 行**）；保留 Cursor 3.31.3 主链路（HTTP/1.1 `BidiAppend(data_binary)` + `RunSSE` + 工具调用 + 长对话压缩 + 多模型路由 + 轻量用量统计）仅需约 **18,500–20,000 行生产代码**，可删除/剥离 **> 85% 代码量**（`server/` 从 `41,112` 行缩至 `≈17,500` 行、`apps/desktop/` `23,938` 行替换为 `apps/menubar/` `≈650` 行、`crates/semble-core/` `3,626` 行全删、`support/benchmarks/semble/` `11,552` 行全删、`protocols/` 精简 `6,917` 行）。
2. **11.5 GB 数据库膨胀真相已查明（非请求/响应体）**：对 `~/.cursor-byok-v3/cursor-byok.db`（`2,811,643` 页 × `4096 B` = `11,516,489,728 B` ≈ `10,983.0 MB`）只读 `dbstat` 实测显示，`llm_detailed_logging` 本就为 `false`（`llm_call_requests = 0`、`llm_call_response_chunks = 0`、`cursor_run_traces = 0`），`llm_calls` 68,259 条仅占 **66.02 MB（0.6%）**。真正撑爆磁盘的是**永不淘汰的 CAS Blob 图与完整对话快照**：`blobs` + `blob_edges`（1,628.8 万条边）占 **7,729.30 MB（70.4%）**、`messages` 占 **1,927.50 MB（17.5%）**、`tool_rounds` + `tool_round_calls` 占 **979.13 MB（8.9%）**、`checkpoint_messages` + `conversation_checkpoints` 占 **294.77 MB（2.7%）**，四者合计占 **99.5%**。
3. **运行期会话改内存 LRU + 单表 `usage.db`**：Cursor 客户端自身持久化会话历史，并在 `AgentRunRequest` 中携带 `conversation_state` / `pre_fetched_blobs`、缺失时响应 `KvServerMessage::GetBlobArgs`。因此轻量版**彻底取消 SQLite 对 `blobs`/`blob_edges`/`messages`/`checkpoints`/`tool_rounds` 的磁盘持久化**，改为进程内有界 LRU 缓存；磁盘仅保留一个轻量的 `~/.cursor-byok-lite/usage.db`（单表 `usage_records`，只存时间、模型、token、TTFT/耗时、状态、错误摘要，默认 90 天/10 万条自动淘汰，长期 `< 30 MB`）。
4. **「6–8 秒同步延迟」根因与提速**：实测 `2026-11-18` 日志 `context_sync` 耗时 `6.19s` / `8.54s`，且 `2026-11-03` 无 `context_sync` 时也有 `4.7s–10.3s` 启动延迟。原因有三：(1) `import_prefetched` 对每个 blob 单独在 11.5 GB SQLite 上执行 `BEGIN IMMEDIATE` 写事务；(2) `context_sync::cache_parts` 用 `prost` 重编码再算 SHA256，容易与 Cursor `@bufbuild/protobuf` 字节序/默认字段不一致导致 `has_missing_part` 频繁失配；(3) 首轮触发 `RequestContextArgs` 等 Cursor 收集规则/MCP。**`context_sync` 不能砍掉**（否则新会话缺失工作区规则与动态 MCP 工具），但通过**去除 SQLite blob 写入 + 以 Cursor 下发的原始 `RequestContextPartReferences` blob_id 为键做内存缓存**，可将重复命中延迟从 `6–8s` 降至 `< 15ms`。
5. **形态选型**：淘汰 Tauri 2 + React/ECharts/Monaco（实测 `.app` **64 MB**，托盘驻留主进程 RSS **43.6–45 MB** + 3 个 WebKit XPC 子进程 **21.4 MB** ≈ **65 MB**，开窗口 `+60–150 MB`），改用仓库 `Cargo.lock` 已有的 **`tray-icon 0.24` + `muda 0.19` + `tao 0.35`**（`ActivationPolicy::Accessory`）单二进制原生 macOS 菜单栏应用，配置文件采用 **TOML**（`~/.cursor-byok-lite/config.toml`），并分 **6 个可独立验收的小步**渐进交付。

---

## 1. 现状盘点与模块去留分析

> **行数统计口径说明**：下表与目录树中，`total` 为实测 `wc -l` 物理总行数；`prod` 为非空、非 `#[cfg(test)]` 模块的源码行数（`.rs` 截止至首个 `#[cfg(test)]` 之前并去除空行；非 Rust 文件去除空行）。不包含 `target/`、`node_modules/`、`package-lock.json` 及 `.png/.icns/.ico/.ttf/.svg` 二进制资源。

### 1.1 现状目录树（实测行数与去留标注）

```text
cursor-byok/                                          # [现状全仓] 排除二进制/lock 后共 172,324 行 (prod 153,365)
├── Cargo.toml                                        # 25 行；workspace 4 members → [简化] 删 semble-core/benchmarks，改 apps/menubar
├── Makefile                                          # 43 行；build-desktop/check → [简化] 对接 apps/menubar 与 .app 打包
├── protocols/cursor/                                 # 60,757 行 (非空 52,979)
│   ├── agent_v1.proto                                # 8,820 行 → [保留] server/build.rs 唯一 prost_build 编译输入
│   ├── aiserver_v1.proto                             # 45,020 行 → [保留] server/build.rs Bidi/ErrorDetails 契约校验文本源
│   └── {anyrun,git_forge,internapi,origin}_v1.proto  # 6,917 行 → [删除] server/build.rs 与 src/ 完全未引用
├── crates/semble-core/                               # 30 files, 3,626 行 (prod 2,468) → [删除] 代码语义索引引擎（见 1.2）
├── support/
│   ├── benchmarks/semble/                            # 15 files, 11,552 行 (prod 11,135) → [删除] semble 基准套件
│   ├── cursor-capture/                               # 22 files, 4,604 行 → [保留] 独立 Go 抓包调试工具（不进产物）
│   └── cursor-protocol-extractor/                    # 13 files, 2,932 行 → [保留] 独立 Go 协议提取工具（不进产物）
├── apps/desktop/                                     # 23,938 行 (prod 22,407) → [删除/替换为 apps/menubar]
│   ├── package.json & vite/ts/i18n plugins           # 8 files, 712 行 → [删除] 前端构建链
│   ├── src/                                          # 152 files, 22,017 行 (prod 20,753) → [删除] React/ECharts/Monaco/Ads/Plugins UI
│   └── src-tauri/src/                                # 9 files, 1,209 行 (prod 998) → [替换] 迁出 tray/startup/limits 到 apps/menubar
├── server/
│   ├── Cargo.toml                                    # 71 行 → [简化] 删 semble-core/serde_yaml/zip/include_dir 等
│   ├── build.rs                                      # 54 行 (prod 49) → [保留] 编译 agent_v1.proto + 断言 aiserver
│   ├── migrations/                                   # 9 files, 616 行 → [删除] 轻量版改用单表 usage.db 自动建表
│   ├── prompt/cursor/                                # 24 files, 1,805 行 → [简化] tools.json & modes/*.json 剔除 Semble 工具
│   ├── plugins/build-in/                             # 27 files, 5,009 行 → [删除] antigravity/codex/grok Deno 内置插件
│   ├── tests/                                        # 18 files, 9,360 行 → [简化] 保留核心协议/压缩/工具/生命周期测试，删 knowledge/external_api 等
│   └── src/                                          # 169 files, 38,374 行 (prod 32,775)
│       ├── lib.rs, bin/cursor-server.rs              # 35 行 → [保留/微调]
│       ├── app.rs                                    # 208 行 (prod 195) → [简化] 去掉 PluginRuntime/ControlService/byok/WebConsole
│       ├── config.rs                                 # 170 行 (prod 139) → [重写/合并] 改为 TOML 配置加载与热重载
│       ├── error.rs                                  # 79 行 (prod 76) → [保留]
│       ├── network.rs                                # 225 行 (prod 180) → [保留/微调] 出站 TLS 与可选代理配置从 TOML 读取
│       ├── api/
│       │   ├── mod.rs, router.rs                     # 14 行 → [保留]
│       │   ├── byok/                                 # 5 files, 1,561 行 (prod 1,503) → [删除] 对外 OpenAI/Anthropic 兼容服务
│       │   └── cursor/                               # 5 files, 1,452 行 (prod 1,124) → [保留/微调]
│       │       ├── handlers.rs                       # 438 行 → [简化] 删 knowledge/tab/commit_message/observability trace 注入，保留 3.31 RunInference 回退
│       │       ├── bidi.rs                           # 547 行 → [保留] 含 3.31 data_binary XOR 解码
│       │       ├── run_sse.rs                        # 232 行 → [简化] 去掉 observability trace 录制
│       │       └── proxy.rs                          # 227 行 → [保留] 透明转发非本地请求至 api2.cursor.sh
│       ├── control/                                  # 9 files, 2,362 行 (prod 2,079) → [删除/拆解]
│       │   ├── ads.rs                                # 408 行 (prod 311) → [删除] tab.leokun.cn 推广拉取（525 噪音源）
│       │   └── service.rs, calls.rs, models.rs, ...  # 1,954 行 → [删除] Web 控制台 API（由菜单栏直接调用本地方法替代）
│       ├── cursor/
│       │   ├── checkpoint/                           # 13 files, 2,097 行 (prod 1,946) → [简化] 保留协议编解码与结构构造，底层脱离 SQLite
│       │   ├── compile/                              # 8 files, 2,147 行 (prod 2,000) → [保留/微调] 删 context.rs::merge_local_rules
│       │   ├── conversation/                         # 7 files, 2,275 行 (prod 2,161) → [保留/微调] 去掉 trace 录制，存储改内存会话状态
│       │   ├── prompting/                            # 5 files, 455 行 (prod 342) → [保留] 系统提示词与工具目录编译
│       │   ├── protocol/                             # 5 files, 642 行 (prod 585) → [保留] connect 5B 帧、events、proto 子集
│       │   ├── services/
│       │   │   ├── knowledge/                        # 3 files, 1,027 行 (prod 798) → [删除] 本地 md 规则镜像与 KnowledgeBase* RPC
│       │   │   ├── observability/                    # 3 files, 577 行 (prod 532) → [删除] cursor_run_traces 录制 worker
│       │   │   ├── tab.rs                            # 51 行 (prod 44) → [删除] Tab 补全自定义服务端转发
│       │   │   ├── commit_message.rs                 # 415 行 (prod 320) → [删除] SCM 提交信息本地生成（改透明直连）
│       │   │   ├── context_sync.rs                   # 197 行 (prod 180) → [简化+提速] 改内存按原始 Part BlobId 缓存，消除 6-8s 重复同步
│       │   │   ├── blob_sync.rs                      # 328 行 (prod 260) → [简化] 对接内存 LRU Blob 缓存 + GetBlobArgs 回源
│       │   │   ├── account.rs                        # 507 行 (prod 400) → [简化] 仅保留必要本地账号/用量 mock（删冗长解析）
│       │   │   ├── analytics.rs                      # 261 行 (prod 220) → [保留] BootstrapStatsig 门控覆写（防切 WebSocket/RunInference）
│       │   │   ├── compatibility.rs                  # 119 行 (prod 75) → [保留] AvailableDocs/EffectivePlugins 等空响应兼容
│       │   │   ├── entitlement.rs                    # 143 行 (prod 75) → [保留] 免费账号鉴权缓存
│       │   │   ├── model_catalog.rs                  # 862 行 (prod 740) → [简化] 删 plugin 模型分支，支持 ConfigStore 默认模型置顶
│       │   │   ├── server_config.rs                  # 51 行 (prod 34) → [保留] 强制 http2_config = 1
│       │   │   └── usage.rs                          # 219 行 (prod 200) → [保留] 上下文 token 占比进度条
│       │   ├── tools/                                # 28 files, 6,792 行 (prod 5,942) → [简化] 删 SembleSearch/SembleFindRelated 分支
│       │   └── transport/                            # 6 files, 719 行 (prod 601) → [保留/微调] 移除 CursorTraceRecorder 绑定
│       ├── local_app/                                # 7 files, 1,156 行 (prod 774) → [保留/微调]
│       │   ├── mod.rs, proxy.rs, settings.rs, ...    # 990 行 → [微调] proxy.rs 删 tab 路径拦截；ca/windows.rs 可删或保留
│       ├── model/                                    # 12 files, 1,848 行 (prod 1,312) → [简化] 删 observability/plugin 字段
│       ├── plugin/                                   # 26 files, 7,220 行 (prod 5,800) → [删除] Deno 运行时、OAuth、插件 SDK
│       ├── provider/                                 # 10 files, 2,953 行 (prod 2,381) → [简化] 删 plugin 路由、详细 chunk 录制与 template 冗余
│       ├── run/                                      # 12 files, 2,950 行 (prod 2,271) → [保留/微调] 保留 engine/compaction/tool_round，存储改内存会话
│       ├── search/                                   # 7 files, 1,420 行 (prod 1,179) → [简化] 删 search_provider.rs (Semble)，保留 WebSearch/WebFetch
│       └── store/                                    # 18 files, 4,506 行 (prod 3,772) → [重写/大幅删除] 删 CAS/checkpoints/messages/traces/migrations，替换为 MemoryStore + UsageStore
```

### 1.2 重点模块「主链路依赖」实证与去留结论

| 模块 | 实测行数 (total / prod) | 主链路是否依赖（调用链 / `grep` 证据） | 处置 | 具体理由 |
| :--- | :--- | :--- | :--- | :--- |
| **`semble` 代码索引**<br>`crates/semble-core/`<br>`support/benchmarks/semble/`<br>`server/src/search/search_provider.rs` | `15,312 / 13,720`（不含 `tools/` 内胶水） | **否**。`grep` 显示仅在 `tools/tool_call_dispatch/mod.rs:67`（`"semblesearch" \| "semblefindrelated"`）、`codec/render.rs:395`、`tool_call_result/search.rs:14`（`provider_identifier: "builtin-semble"`）及 `prompt/cursor/tools.json:114-175` 出现。并非 Cursor 原生工具，Cursor 自身已有 `Grep`/`Glob`/`Read` 且自带客户端索引。 | **删除** | 移除 `crates/semble-core`、`support/benchmarks/semble`、`search_provider.rs` 及 `tools.json`/`modes/*.json` 中的 `Semble*` 定义，同时摆脱 `tree-sitter`×8、`tokenizers`（`onig` C++ 库）、`safetensors`、`rayon` 重型依赖，大幅缩减二进制体积与编译耗时。 |
| **`knowledge` rules**<br>`server/src/cursor/services/knowledge/` | `1,027 / 798` | **否**。`grep` 仅见于 `api/cursor/handlers.rs:150-163` 的 4 条 `KnowledgeBase{Add,List,Update,Remove}` 路由，以及 `compile/context.rs:152` 的 `merge_local_rules`（读取 `~/.cursor-byok-v3/rules`）。工作区 `.cursor/rules`、`AGENTS.md`、`SKILL.md` 均由 Cursor 客户端通过 `RequestContext.rules`/`agent_skills` 传入。 | **删除** | 仅用于镜像 Cursor 云端 Memories。删除 `knowledge/` 与 `merge_local_rules`；将 `KnowledgeBaseList` 等 4 个路径改为走 `compatibility.rs` 返回空列表或直接删除本地拦截，零影响 Agent 对话与工作区规则。 |
| **`checkpoint` 协议与状态**<br>`server/src/cursor/checkpoint/`<br>(`+ store/checkpoints.rs` 等) | 协议构造 `2,097 / 1,946`；DB 持久层 `~1,800` | **协议层：强依赖；SQLite 持久层：不依赖**。`compile/run.rs:80` 调用 `hydrate_messages(request.conversation_state)` 恢复历史消息；`conversation/output.rs` 在工具轮与结束时构造 `ConversationCheckpointUpdate` 并通过 `BlobSynchronizer::persist`（`SetBlobArgs`）发给 Cursor 持久化到 Cursor 本地。 | **协议保留；DB 持久层删除** | Cursor 客户端本身保存权威 `ConversationStateStructure` 与所有 blob（并在下次请求通过 `pre_fetched_blobs` 回传，缺失时响应 `GetBlobArgs`）。保留 `cursor/checkpoint/`（简化掉 trace 埋点），把底层存储从 SQLite `blobs`/`blob_edges`/`checkpoints` 改为进程内有界 LRU 缓存。 |
| **`context_sync`**<br>`server/src/cursor/services/context_sync.rs` | `197 / 180` | **强依赖**。`compile/run.rs:122` → `compile/context.rs:20` `context::hydrate`：Cursor 发送 `request_context_parts`（只含 `rules/skills/subagents/mcps_blob_id` 哈希），若不命中缓存则必须发 `ExecServerMessage(RequestContextArgs)` 拉取规则、环境、MCP 工具列表。 | **保留并重写缓存** | 不可删除。改为按 Cursor 下发的原始 `RequestContextPartReferences` 的 4 个 `blob_id` 直接做内存缓存，并移除 SQLite `put_blob`，消除跨会话/跨轮 6–8s 重复同步。 |
| **Tab 补全**<br>`server/src/cursor/services/tab.rs`<br>`apps/desktop/.../TabSettingsCard.tsx` | `51 / 44`（服务端）+ `168`（前端） | **否**。`tab.rs` 仅注册 `TAB_PATHS` 16 条路径（`StreamCpp`、`CppService/*`、`FileSyncService/*`），按 `tab_settings` 转发到第三方 Tab 服务或上游。`local_app/proxy.rs:198` `should_route_locally` 命中 `is_tab_path` 才会把流量引给 `cursor-server`。 | **删除** | 从 `local_app/proxy.rs` 的 `should_route_locally` 剔除 `TAB_PATHS`，Tab 流量直接在 `CursorRelay` 透明透传（或直连上游），同时避免默认 `"public"` 模式导致本地伪造 token 请求上游刷出 `525` 错误与 7–15s 阻塞。 |
| **广告系统 `ads`**<br>`server/src/control/ads.rs`<br>`apps/desktop/src/shell/ads/` | `408 / 311`（服务端）+ `554`（前端） | **否**。仅服务 `/__byok-api__/api/promotions` 前端推广弹窗，无任何 Cursor 协议引用。 | **删除** | 消除对 `tab.leokun.cn` 的外网请求、本地 `~/.cursor-byok-v3/ads` 缓存及日志每分钟刷屏的 `525 Bad Gateway` 告警。 |
| **插件系统 `plugins`**<br>`server/src/plugin/`<br>`server/plugins/build-in/`<br>`apps/desktop/src/features/plugins/` | `7,220 / 5,800`（Rust+SDK）+ `5,009`（内置插件）+ `1,255`（前端） | **否（在移除 Deno 插件模型需求后）**。`grep` 显示仅在 `handlers.rs:288`、`provider/router.rs:63`、`model_catalog.rs:255`、`control/service.rs:175` 作为与内置 OpenAI/Anthropic 模型并列的可选分支出现。 | **删除** | 去掉 Deno 运行时下载管理（`installation.rs`）、JS Worker、OAuth 回调 server 与 3 套内置认证插件，使 `ProviderRouter` 只保留原生 `OpenAiChat` / `OpenAiResponses` / `Anthropic`。 |
| **外部兼容接口 `api/byok`**<br>`server/src/api/byok/` | `1,561 / 1,503` | **否**。`app.rs:55` 挂载 `/byok/v1/{models,chat/completions,responses,messages}` 对外转发接口，与 Cursor 接管主链路完全独立。 | **删除** | 用户需求聚焦 Cursor 菜单栏接管，剔除外部 API server 可减 1,561 行及对应的 `external_api_settings`。 |
| **本地 Commit / 详细追踪**<br>`commit_message.rs`、`observability/`、`store/cursor_traces.rs` | `1,337 / 1,152`（不含表） | **否**。`commit_message` 仅处理 `WriteGitCommitMessage`（默认空配置即直连）；`observability` 仅在 `detailed=true` 时录制全量二进制 trace。 | **删除** | 删除 `observability/` 与 `cursor_traces.rs`；`WriteGitCommitMessage` 不再本地拦截，直接透传官方或跟随 Cursor 默认行为。 |

### 1.3 模块去留代码量汇总

| 目录 / 分类 | 现有物理行 (`wc -l`) | 拟删除 / 替换行 | 拟保留 / 简化后估算 (`prod`) | 变化幅度 |
| :--- | ---: | ---: | ---: | ---: |
| `crates/semble-core/` | 3,626 | -3,626 | 0 | -100% |
| `support/benchmarks/semble/` | 11,552 | -11,552 | 0 | -100% |
| `support/cursor-{capture,protocol-extractor}/` | 7,536 | 0（保留作开发工具） | 7,536（不进产物） | 0% |
| `protocols/cursor/` | 60,757 | -6,917（4 个未用 proto） | 53,840（只编译 `agent_v1.proto`） | -11.4% |
| `apps/desktop/` → `apps/menubar/` | 23,938 | -23,938（新增 ≈650） | ≈650 | -97.3% |
| `server/plugins/build-in/` | 5,009 | -5,009 | 0 | -100% |
| `server/migrations/` | 616 | -616 | 0（单表运行时建表） | -100% |
| `server/src/` | 38,374 | ≈-20,500 | ≈16,500（prod） | -57% |
| `server/prompt/` | 1,805 | ≈-85（删 Semble 工具声明） | ≈1,720 | -4.7% |
| `server/tests/` | 9,360 | ≈-3,800（删被移除模块测试） | ≈5,500 | -41% |
| **合计（含测试与支持工具）** | **172,324** | **≈-76,940** | **—** | **主程序 prod 代码量（含 server+menubar，不含 proto）从 ≈56,180 降至 ≈17,150（降 69.5%）** |

---

## 2. 11.5 GB 数据库膨胀原因实证

对本机 `~/.cursor-byok-v3/cursor-byok.db` 执行严格只读检查（`sqlite3 -readonly` `dbstat` 及 `?immutable=1` 行数/载荷统计，未写入任何字节）：
- **文件物理页统计**：`page_size = 4096`，`page_count = 2,811,643`，`freelist_count = 0`，总计 **`11,516,489,728` 字节（`10,983.0 MB` / `10.73 GiB` / `11.5 GB`）**。

### 2.1 各表与索引实测体积及行数分布

| 逻辑表群 | 明细（表 + 索引 `dbstat` 实测） | 占用体积 (MB) | 占总库比例 | 行数（`immutable=1` 实测） | 原始载荷字节 (`SUM(length)`) | 是否必须长期落盘 |
| :--- | :--- | ---: | ---: | ---: | ---: | :--- |
| **`blob_edges`** | 表 `1,719.60` + 主键索引 `2,049.84` + `blob_edges_child` `762.77` | **4,532.21** | **41.27%** | **16,288,438** | —（纯 32B×2 外键关系） | **否**（`blob_closure` 全仓零生产调用） |
| **`blobs`** | 表 `3,177.42` + 主键索引 `19.07` | **3,196.49** | **29.10%** | **434,036** | 3,094,134,458 B (2.88 GiB) | **否**（Cursor 已持久化，可用内存 LRU + `GetBlobArgs` 回源） |
| **`messages`** | 表 `1,885.05` + 主键索引 `41.99` + `messages_runtime_event` `0.46` | **1,927.50** | **17.55%** | **289,475** | 1,669,282,324 B (1.55 GiB) | **否**（仅单次对话内存使用，历史由 Cursor `conversation_state` 带回） |
| **`tool_rounds`** | 表 `671.63` + 主键索引 `4.62` + `tool_rounds_run_status` `4.40` | **680.65** | **6.20%** | **63,667** | 636,229,628 B (606.8 MB) | **否**（仅单轮工具恢复期间有效） |
| **`tool_round_calls`** | 表 `271.80` + 3 个索引 `22.01` | **298.48** | **2.72%** | **83,766** | 196,374,679 B (187.3 MB) | **否**（含工具大参数与完整 `result_content`） |
| **`checkpoint_messages`** | 表 `142.88` + 2 个索引 `125.64` | **268.52** | **2.44%** | **1,241,686** | — | **否** |
| **`llm_calls`** | 表 `47.18` + 4 个索引 `18.84` | **66.02** | **0.60%** | **68,259** | 12,658,099 B (12.1 MB) | **简化保留**（仅保留轻量字段 + TTL 清理） |
| **`conversation_checkpoints`** | 表 `7.87` + 2 个索引 `12.89` | **20.76** | **0.19%** | **91,249** | — | **否** |
| **`runs` / `input_anchors` / `conversations` / `model_configs` / `service_settings`** | 其余全部小表及索引 | **1.87** | **0.02%** | `2,615` / `1,818` / `404` / `14` / `4` | — | **除模型配置与统计外均不需落盘** |
| **`llm_call_requests` / `llm_call_response_chunks` / `cursor_run_traces*`** | 详细录制表 | **0.02** | **0.00%** | **0** | 0 B | **删除**（本次用户本就关闭了详细日志） |

### 2.2 核心结论

1. **99.3% 的体积（10,916 MB）来自“零淘汰的 CAS Blob + 对话检查点历史”**：`server/src/store/cas.rs` 与 `checkpoints.rs`、`messages.rs`、`tool_rounds.rs` 只有 `INSERT`，没有任何 `DELETE`/TTL/容量上限；而且每保存一个 `ConversationStateStructure` 都会向 `blob_edges` 写入整棵父子边集，累积了 **1,628.8 万条边记录**，仅 `blob_edges` 一张表及其两个 B-Tree 索引就高达 **4.53 GB**（甚至超过 `blobs` 本体 3.20 GB）。更关键的是，经 `grep` 验证 `Store::blob_closure` 在 `server/src/` 中**没有任何生产代码调用它**。
2. **`llm_calls` 本身非常轻量**：68,259 次调用记录（包含当前多余的 `usage_json`、`provider_url`、`request_url`、`model_hash` 等 40 列）也只占 **66.02 MB**（平均约 1 KB/条）。若裁成轻量版单表并加上 `90 天 / 10 万条` 上限，长期占用将稳定在 **< 30 MB**。

---

## 3. 轻量版目标架构设计

### 3.1 目标目录树与估算行数（不含测试、空白行）

```text
cursor-byok/                                          # [目标结构] prod 合计 ≈17,150 行（不含 protocols/）
├── Cargo.toml                                        # ≈15 行；workspace = ["server", "apps/menubar"]
├── Makefile                                          # ≈28 行；check / build-app / dmg（arm64）
├── protocols/cursor/
│   ├── agent_v1.proto                                # 8,820 行；prost_build 唯一编译输入
│   └── aiserver_v1.proto                             # 45,020 行；build.rs 契约校验输入
├── apps/menubar/                                     # ≈650 行 (prod)；纯 Rust 原生 macOS 菜单栏单二进制
│   ├── Cargo.toml                                    # ≈35 行；cursor-server, tray-icon, muda, tao, tokio, libc
│   ├── Info.plist & assets/trayTemplate.png          # macOS Accessory App 描述与黑白模板图标（LSUIElement=true）
│   └── src/
│       ├── main.rs                                   # ≈140 行；flock 单实例锁、日志轮转初始化、tao EventLoop + tokio 运行时
│       ├── tray.rs                                   # ≈340 行；tray-icon + muda 菜单构建、动态刷新状态/今日本月用量/切换默认模型/打开文件/退出
│       ├── actions.rs                                # ≈120 行；开/关接管、open config.toml / logs / 导出 CSV 保存与打开
│       └── resource_limits.rs                        # ≈50 行；提升 macOS RLIMIT_NOFILE
└── server/
    ├── Cargo.toml                                    # ≈42 行；去掉 semble-core/serde_yaml/zip/include_dir/rayon/tree-sitter
    ├── build.rs                                      # 54 行；不变
    ├── prompt/cursor/                                # ≈1,720 行；从 tools.json & modes/*.json 剔除 Semble*
    ├── tests/                                        # ≈5,500 行；保留 connect_wire/compaction/tool_round/prefix_stability/cursor_transport_lifecycle 等
    └── src/                                          # ≈16,500 行 (prod)
        ├── lib.rs                                    # ≈18 行；导出 App, ConfigStore, UsageStore, CursorHarness
        ├── app.rs                                    # ≈120 行；组装 ConfigStore + MemoryStore + UsageStore + ProviderRouter + TransportRegistry + CursorHarness
        ├── error.rs                                  # ≈75 行；HTTP 映射与错误分类
        ├── network.rs                                # ≈180 行；复用 reqwest native-tls 客户端与 TOML 出站代理配置
        ├── config/
        │   ├── mod.rs                                # ≈40 行；~/.cursor-byok-lite 路径、TOML 读取与运行期热重载
        │   ├── schema.rs                             # ≈220 行；config.toml 结构（providers/models/server/stats）、env:VAR 解析、model_hash 计算
        │   └── export_db.rs                          # ≈160 行；一次性只读迁移工具：从 ~/.cursor-byok-v3/cursor-byok.db 导出 14 个模型至 config.toml
        ├── api/
        │   ├── mod.rs, router.rs                     # ≈15 行
        │   └── cursor/                               # ≈960 行；bidi.rs (data_binary XOR), handlers.rs (含 RunInference 回退), run_sse.rs, proxy.rs
        ├── cursor/
        │   ├── checkpoint/                           # ≈1,750 行；保留 ConversationStateStructure 编解码与构造（去 trace 埋点，对接 MemoryStore）
        │   ├── compile/                              # ≈1,950 行；AgentRunRequest 编译（删 merge_local_rules）
        │   ├── conversation/                         # ≈1,950 行；ConversationRuntime & ConversationOutput（去 trace，读写 MemoryStore）
        │   ├── prompting/                            # ≈340 行；内置提示词与工具目录编译
        │   ├── protocol/                             # ≈580 行；Connect 5B 帧、proto.rs、events.rs
        │   ├── services/
        │   │   ├── account.rs, analytics.rs,         # ≈1,450 行；删 knowledge/observability/tab/commit_message
        │   │   │   compatibility.rs, entitlement.rs, #     analytics.rs 保持 nal_websocket_client=false / agent_host_local_loop=false
        │   │   │   server_config.rs, usage.rs,       #     model_catalog.rs 支持 ConfigStore.default_model 置顶
        │   │   │   model_catalog.rs
        │   │   ├── blob_sync.rs                      # ≈240 行；MemoryStore + KvServerMessage::GetBlobArgs/SetBlobArgs
        │   │   └── context_sync.rs                   # ≈150 行；按原始 RequestContextPartReferences blob_id 缓存，零落盘
        │   ├── tools/                                # ≈5,600 行；删 SembleSearch/SembleFindRelated，保留全部读写/终端/MCP/WebFetch/WebSearch
        │   └── transport/                            # ≈520 行；去掉 CursorTraceRecorder
        ├── local_app/                                # ≈700 行；CursorHarness、Hudsucker MITM (52148)、CA、settings.json、pkill；proxy.rs 删 TAB_PATHS
        ├── model/                                    # ≈1,050 行；CanonicalMessage、ModelConfig、UsageSummary、PreparedRun（删 plugin/observability 类型）
        ├── provider/                                 # ≈1,850 行；OpenAiChat、OpenAiResponses、Anthropic、ProviderRouter（删 plugin 与 chunk 录制）
        ├── run/                                      # ≈2,150 行；RunEngine、compaction（长对话压缩）、tool_round、model_cycle（读写 MemoryStore）
        ├── search/                                   # ≈1,040 行；WebSearch & WebFetch（删 search_provider.rs / Semble）
        └── store/
            ├── mod.rs                                # ≈25 行
            ├── memory.rs                             # ≈450 行；有界 LRU BlobCache(256MB) + 活跃会话内存状态（messages/tool_rounds/context_usage_anchor）
            └── usage.rs                              # ≈320 行；~/.cursor-byok-lite/usage.db（单表 usage_records、今日/本月汇总、自动清理、CSV 导出）
```

### 3.2 完整主执行路径（数据流与控制流）

```text
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ Cursor 3.31.3 (settings.json: http.proxy=127.0.0.1:52148, disableHttp2=true)             │
│  ├─ GetServerConfig ─► http2_config=1 / BootstrapStatsig ─► 禁用 WebSocket & RunInference│
│  ├─ AvailableModels / GetDefaultModel ─► ConfigStore 读取 providers+models+default_model │
│  └─ 对话主链路: 并行 POST BidiAppend(data_binary=AgentClientMessage) + POST RunSSE       │
└───────────────────────────────┬──────────────────────────────────────────▲───────────────┘
                                │ TLS MITM (api2.cursor.sh:443)            │
                                ▼                                          │
┌──────────────────────────────────────────────────────────────────────────┴───────────────┐
│ local_app/proxy.rs :: CursorRelay (127.0.0.1:52148)                                      │
│  ├─ is_local_path(path) ──► 改写至 http://127.0.0.1:<service_port> (默认 51866)          │
│  └─ 其他路径（含原 TAB_PATHS/WriteGitCommitMessage）──► 透明直通官方上游 api2.cursor.sh  │
└───────────────────────────────┬──────────────────────────────────────────▲───────────────┘
                                │ HTTP/1.1                                 │
                                ▼                                          │
┌──────────────────────────────────────────────────────────────────────────┴───────────────┐
│ api/cursor/handlers.rs                                                                   │
│  ├─ RunInference* ──► 拒绝(permission_denied)触发 Cursor 立即回退 RunSSE                │
│  ├─ bidi_handler ──► data XOR data_binary 解码 ──► model_hash 查 ConfigStore             │
│  │    ├─ 命中本地模型 ─► TransportRegistry.get_or_create_for_append ──► ConversationRuntime│
│  │    └─ 官方模型 ─────► mark_upstream + proxy::forward                                  │
│  └─ run_sse_handler ─► wait_route ─► 订阅 OutputHub 推送 Connect 5B 帧与 END_STREAM(0x02)│
└───────────────────────────────┬──────────────────────────────────────────▲───────────────┘
                                │                                          │
                                ▼                                          │
┌──────────────────────────────────────────────────────────────────────────┴───────────────┐
│ ConversationRuntime + compile + run::RunEngine                                           │
│  ├─ compile::prepare:                                                                    │
│  │   ├─ import_prefetched ──► MemoryStore.blobs (LRU 256MB, 零 SQLite 写入)             │
│  │   ├─ hydrate_messages  ──► 优先 MemoryStore，未命中发 GetBlobArgs 向 Cursor 客户端拉取 │
│  │   └─ context_sync      ──► 按原始 part blob_id 命中内存缓存(<1ms)，缺失发 RequestContextArgs│
│  ├─ run::RunEngine:                                                                      │
│  │   ├─ 长对话压缩 (compaction.rs): 依 context_window - 10% 预算或 400 溢出自动摘要裁剪   │
│  │   ├─ ProviderRouter ──► OpenAiChat / OpenAiResponses / Anthropic (API Key 支持 env:)  │
│  │   └─ ToolDispatcher ──► ExecServerMessage(Shell/Read/Edit/Grep/MCP) / WebSearch/Fetch │
│  ├─ CheckpointBuilder ──► SetBlobArgs + ConversationCheckpointUpdate 回写 Cursor 持久化  │
│  └─ UsageRecorder ──► 异步单行写入 usage.db (ts, model, tokens, ttft_ms, duration_ms, status)│
└──────────────────────────────────┬───────────────────────────────────────────────────────┘
                                   │ 菜单打开/定时刷新读取汇总 & 更新托盘状态
                                   ▼
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ apps/menubar (原生 macOS 菜单栏 Accessory 进程: tray-icon + muda + tao)                    │
│  菜单: 状态(接管中/端口/默认模型/最近错误) | 今日·本月 Token | 开关接管 | 默认模型▶ | 编辑配置 | 导出CSV | 打开日志 | 退出│
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

### 3.3 配置文件格式选型与 14 模型一次性导出方案

#### 选型结论：**采用 TOML（`~/.cursor-byok-lite/config.toml`）**
| 对比维度 | **TOML（推荐）** | JSON |
| :--- | :--- | :--- |
| 人类直接编辑与阅读 | **原生支持 `#` 注释**，可用 `#` 临时注释某个模型/供应商，无需为字符串加双引号键名，无尾逗号语法陷阱。 | 不支持注释（若用 JSON5 则失去标准工具校验，手写极易因漏逗号/引号报错）。 |
| 多模型+多供应商组织 | `[[providers]]` 与 `[[models]]` 分块天然适合数组表，结构平铺，14 个模型一目了然。 | 大量嵌套 `{}` 与缩进，单文件超过百行后视觉噪音大。 |
| 敏感凭据引用 | 原生支持 `api_key = "env:DEEPSEEK_API_KEY"` 或直接明文，注释可写示例。 | 同等能力但无法在字段旁加 `# 环境变量名` 提示。 |
| Rust 生态依赖 | `toml = "0.8"`（配合已有 `serde`）零额外 C 依赖，编译增量极小。 | `serde_json` 已有。 |

**`~/.cursor-byok-lite/config.toml` 规范示例**：
```toml
default_model = "deepseek-v4.1-flash"   # 可填 display_name 或 16 位 model_hash

[server]
listen_port = 51866                     # 0 表示随机端口
proxy_port = 52148
request_timeout_secs = 3600
stream_idle_timeout_secs = 1800
# outbound_proxy = "http://127.0.0.1:7890"  # 可选：访问上游模型时的出站代理

[stats]
retention_days = 90                     # 用量明细保留天数，0 表示不按天数删
max_rows = 100000                       # 最大保留条数，超出按最旧时间淘汰

[[providers]]
name = "lan-7863"                       # 供应商标识，供下方 [[models]] 引用
type = "openai"                         # "openai" | "anthropic"
base_url = "http://192.168.11.46:7863/v1"
api_key = "env:LAN_7863_API_KEY"        # 支持 "env:ENV_VAR" 或直接明文

[[providers]]
name = "ziyouxie"
type = "openai"
base_url = "https://api.ziyouxie.online"
api_key = "env:ZIYOUXIE_API_KEY"

[[models]]
provider = "lan-7863"
display_name = "deepseek-v4.1-flash"
model_id = "deepseek-v4.1-flash"
endpoint = "/v1/chat/completions"       # OpenAI: "/v1/chat/completions" | "/v1/responses"
# model_hash = "83f1333d63840a0c"       # 可选固定；省略时按原 Sha256(url,model_id,key,display,endpoint) 计算
# reasoning_effort = "medium"           # 可选: low | medium | high
# context_window_tokens = 128000
# max_completion_tokens = 16384
```
> **兼容关键点**：现有 `~/.cursor-byok-v3/cursor-byok.db` 中已有 14 个模型，Cursor 客户端聊天历史与默认选择记录的是 16 位 hex `model_hash`（由 `server/src/model/configuration.rs:285` 的 `Sha256(request_url \n model_id \n api_key \n display_name [\n openai_endpoint])[..8]` 算得）。因此允许在 `[[models]]` 中保留/自动生成相同的 `model_hash`，这样切换轻量版后，**Cursor 中现有历史对话与模型选择器无需重新配对**。

#### 一次性从 `~/.cursor-byok-v3/cursor-byok.db` 导出 14 个模型配置的方案
在正式代码中提供子命令 `cursor-byok export-models`（或 Step 3 验证阶段先用只读 Python 脚本验证），**以只读模式（`mode=ro&immutable=1`）读取 `model_configs`**：
1. 按 `(model_type, base_url, api_key, custom_headers_json)` 归并去重生成 `[[providers]]`（如本机 14 个模型可归并为约 5 个供应商：`192.168.11.46:7863`、`localhost:7863`、`api.ziyouxie.online`（OpenAI/Anthropic 各一或按 key 分）、`127.0.0.1:8899`、`192.168.11.46:8000`）；
2. 将 14 行按 `sort_order, display_name` 输出为 `[[models]]`，保留原 `model_hash`（确保即使后续用户把明文 `api_key` 改为 `env:VAR` 也不会改变 Cursor 识别的 `model_hash`），并默认把第一条或用户指定模型设为 `default_model`；
3. 读取 `service_settings` 中的 `network_ports`（`51866` / `52148`）写入 `[server]`；
4. 若 `~/.cursor-byok-v3/ca` 已存在且已在系统钥匙串受信任，初次启动时**只读拷贝 `ca/` 到 `~/.cursor-byok-lite/ca/`**（文件权限 `0700`/`0600`），避免重新提示用户输入系统密码安装 CA 证书。

### 3.4 统计存储设计（`~/.cursor-byok-lite/usage.db`）

只建一张表 `usage_records`（配合 `PRAGMA auto_vacuum = INCREMENTAL` 与 `journal_mode = WAL`），**严禁存储请求头/体、响应体、SSE chunk、Blob 或消息内容**：

```sql
CREATE TABLE IF NOT EXISTS usage_records (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms             INTEGER NOT NULL,          -- 调用结束时间戳 (UTC ms)
    model_hash        TEXT NOT NULL,
    model_id          TEXT NOT NULL,
    display_name      TEXT NOT NULL,
    provider_name     TEXT NOT NULL,
    input_tokens      INTEGER NOT NULL DEFAULT 0,
    output_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens  INTEGER NOT NULL DEFAULT 0,
    total_tokens      INTEGER NOT NULL DEFAULT 0,
    ttft_ms           INTEGER,                   -- 首字延迟
    duration_ms       INTEGER NOT NULL DEFAULT 0,
    status            TEXT NOT NULL,             -- 'ok' | 'cancelled' | 'error'
    error_summary     TEXT                       -- 截断至 <= 256 字符
);
CREATE INDEX IF NOT EXISTS idx_usage_ts ON usage_records(ts_ms DESC);
```
- **体积估算**：每条记录约 `150–200 B`（含 B-Tree 与 1 个索引）。按 `max_rows = 100,000` 上限计算，即便存满 10 万条也仅 **~20 MB**（此前 6.8 万条无清理是 11.5 GB 的 `0.17%`）。
- **自动清理**：每次启动与每满 500 次写入触发一次 `DELETE FROM usage_records WHERE ts_ms < now - retention_days*86400_000` 及超出 `max_rows` 的旧行裁剪，随后执行 `PRAGMA incremental_vacuum(256)`。
- **统计查询与 CSV 导出**：
  - 菜单栏打开/每次写入后内存增量更新 `今日 Token`（本地时区 `00:00` 起）与 `本月 Token`（本地自然月 1 日起）及最近一次错误摘要（含时间戳与截断文案）。
  - 点击菜单「导出统计 CSV…」弹出原生保存面板（或直接写入 `~/.cursor-byok-lite/exports/usage-YYYYMMDD-HHMMSS.csv` 并在 Finder 中高亮显示），流式导出 `ts_iso,model_id,display_name,provider_name,input_tokens,output_tokens,cache_read_tokens,reasoning_tokens,total_tokens,ttft_ms,duration_ms,status,error_summary`。

### 3.5 `context_sync` 与启动路径「6–8 秒延迟」分析及提速方案

1. **现状实测数据**：
   - `2026-11-18` 日志：`10:20:50.121` (`context_sync`) → `10:20:58.666` (`Run claimed`) 耗时 **8.545s**；`10:37:52.454` → `10:37:58.644` 耗时 **6.190s**。
   - `2026-11-03` 日志（未打 `context_sync` 日志的续轮）：`02:02:04.174` → `02:02:14.454` **10.28s**、`02:55:55.568` → `02:56:01.304` **5.74s**、`03:35:48.872` → `03:35:55.514` **6.64s**。
2. **根因定位（`compile/run.rs:70-125` & `context_sync.rs:149-196`）**：
   - **(A) 无 `context_sync` 也很慢**：每次 `AgentRunRequest` 都调用 `checkpoint.import_prefetched(&request.pre_fetched_blobs)`，对几十上百个 blob 逐条在 11.5 GB SQLite 上执行 `BEGIN IMMEDIATE` + `INSERT INTO blobs`；随后 `import_checkpoint` 还要加载并校验 `conversation_checkpoints` 谱系。
   - **(B) `context_sync` 频繁 cache miss**：`context_sync::cache_parts` 把 `RequestContext` 拆成 4 个 `RequestContext*Part`，再用 `prost::Message::encode_to_vec()` 重编码并计算 `Sha256` 存库；一旦 Cursor 客户端 `@bufbuild/protobuf` 的字段顺序、可选空字段编码与 `prost` 有 1 字节差异，算出的 `BlobId` 就不等于 Cursor 下发的 `parts.rules_blob_id / mcps_blob_id`，导致**新对话或新窗口再次 miss**，触发 `ExecServerMessage(RequestContextArgs)` 等待 Cursor 扩展扫描工作区规则与 MCP 工具。
3. **提速方案（保留功能，砍掉延迟）**：
   - **消除磁盘写入瓶颈**：`import_prefetched` 与 `blobs` 改写入内存 `MemoryBlobCache`（`RwLock<LruCache>`），N 次磁盘事务变成微秒级内存插入；
   - **修正 `context_sync` 缓存键**：将 `RequestContextSynchronizer::refresh_if_missing` 改为**直接以 Cursor 下发的 `RequestContextPartReferences` 中的原始 `rules_blob_id / skills_blob_id / subagents_blob_id / mcps_blob_id` 为键**，缓存已解析的 `RequestContext` 分片（内存 LRU，上限如 256 组），**不再依赖 `prost` 重编码哈希反推**；同时在 `ConversationRuntime` 内保留最近一次 `RequestContext` 以供 `BackgroundTaskCompletionAction` 复用；
   - **预期效果**：同工作区首次冷请求仍需等待 Cursor 返回 `RequestContextResult`（约 1–3s，不含 11.5GB 写库阻塞），第 2 次及所有后续轮次/新对话的本地准备延迟降至 **`< 5 ms`**。

### 3.6 菜单栏实现选型（实测对比 + 估算）与量化目标

#### 现有方案实测数据（2026-11-18 本机实测）
- **安装包体积**：原 `x86_64` `/Applications/Cursor助手.app` = **71 MB**；阶段 1 `arm64` `Cursor BYOK.app` = **64 MB**（主二进制 `cursor-byok-desktop` 占 `64 MB`，`Resources` 占 `120 KB`）。
- **常驻内存**：`PID 7402`（窗口关闭仅留托盘状态）`ps` RSS = **43.6–45 MB**（`top` MEM = `148 MB`），伴随 3 个未退出的 WebKit XPC 进程（`WebContent` `4.8 MB` + `Networking` `7.2 MB` + `GPU` `9.4 MB` = **21.4 MB**），合计常驻 **~65 MB RSS**（打开窗口时再增 `60–150 MB`）。

#### 候选方案对比
| 方案 | 架构方式 | `.app` 体积 | 常驻内存 (RSS) | 冷启动耗时 | 优劣评估 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **A. 保留 Tauri 仅用 Tray** | 删 React 前端与窗口，仍保留 `tauri` + `wry` + `tao` + 插件 | 估算 `~28–35 MB` | 估算 `~30–40 MB` | 估算 `~0.5 s` | 仍链接 WebKit/Tauri 运行时与多项无用插件，代码和构建包仍有明显冗余。 |
| **B. 纯 Rust 原生菜单栏（推荐）** | 单二进制 `apps/menubar`：直接复用 `Cargo.lock` 已有 **`tray-icon` + `muda` + `tao`**（`ActivationPolicy::Accessory` 无 Dock 图标）+ 内嵌 `cursor-server` | **估算 `< 18 MB`**（DMG `< 8 MB`） | **估算 `~15–22 MB`**（零 WebKit 子进程） | **估算 `< 0.3 s`** | 符合 `AGENTS.md` 最简原则与现有依赖复用，单进程无 IPC，无浏览器引擎内存开销，维护单一 Cargo 构建。 |
| **C. Swift 原生托盘 + Rust Sidecar** | Swift `MenuBarExtra` 进程 + 拉起 `cursor-server` | 估算 `~18–22 MB` | 估算 `~25–32 MB`（双进程） | 估算 `~0.4 s` | 需维护 Swift/Xcode + HTTP/Unix Socket 进程管理与生命周期同步，复杂度高于单二进制。 |

#### 轻量版量化目标（验收基线）
- **`.app` 安装包体积**：**`< 20 MB`**（较现状 `64 MB` 降低 **> 68%**；DMG `< 10 MB`）
- **托盘常驻内存（RSS）**：**`< 25 MB`**（较现状 `~65 MB` 降低 **> 61%**，无 WebKit XPC 子进程）
- **冷启动时间**：从进程拉起到菜单栏图标就绪且 `127.0.0.1:51866` 开始监听 **`< 0.4 s`**
- **代理首 token 额外延迟**：命中 `RequestContext` 内存缓存时，从 `BidiAppend(seqno=0)` 收到请求到发出上游模型 HTTP 请求的本地处理耗时 **`< 15 ms`**（不含首次跨进程 `RequestContextArgs` 及上游模型网络 RTT）
- **持久化数据库体积上限**：`usage.db` 长期 **`< 30 MB`**（较现状 `11.5 GB` 降低 **> 99.7%**）

---

## 4. 分步实施计划（6 个可独立验收的小步）

> 排序原则：**先删不影响主链路的旁路模块 → 再删插件与重型录制 → 再换 TOML 配置 → 再替换存储层（内存 LRU + 轻量 `usage.db`）→ 再切换菜单栏形态 → 最后清理打包与量化验收**。每一步结束后仓库均可独立编译、跑通单测/集成测试，并在本机实测 Cursor 3.31.3 + `deepseek-v4.1-flash` 对话通过。

### Step 1：删除旁路独立模块（Semble、Knowledge、Ads、Tab、External BYOK API、Unused Protos）
- **改动范围**：
  - 删除目录/文件：`crates/semble-core/`、`support/benchmarks/semble/`、`server/src/search/search_provider.rs`、`server/src/cursor/services/knowledge/`、`server/src/control/ads.rs`、`server/src/cursor/services/tab.rs`、`server/src/api/byok/`、`protocols/cursor/{anyrun,git_forge,internapi,origin}_v1.proto`、`apps/desktop/src/shell/ads/`（或前端对应占位空数组）、`apps/desktop/src/features/settings/TabSettingsCard*`。
  - 修改：`Cargo.toml` 移除 `semble-core` 与 `benchmarks/semble`；`server/Cargo.toml` 移除 `semble-core` 依赖；`server/prompt/cursor/tools.json` 与 `modes/*.json` 移除 `SembleSearch`、`SembleFindRelated`；`server/src/cursor/tools/` 移除 `semble*` 分支；`server/src/cursor/compile/context.rs` 移除 `merge_local_rules`；`server/src/local_app/proxy.rs` 从 `should_route_locally` 移除 `TAB_PATHS`；`server/src/api/cursor/handlers.rs` 将 `KnowledgeBase*` 改为 `compatibility.rs` 空响应或删除本地路由，移除 `tab::router()`；`server/src/app.rs` 不再挂载 `byok` 路由；`server/tests/` 移除 `external_api.rs`、`knowledge_rules.rs`、`local_rules_context.rs`。
- **验收命令与预期结果**：
  ```bash
  cargo test --workspace --all-targets
  TAURI_SIGNING_PRIVATE_KEY=.tauri/cursor-byok.local.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
    npm --prefix apps/desktop run tauri:build -- --target aarch64-apple-darwin --bundles app
  ```
  - 预期：`cargo test` 全绿；Cursor 3.31.3 发起 `deepseek-v4.1-flash` 对话与工具调用正常完成；日志不再出现 `tab.leokun.cn` 525 告警及 `CppService` 拦截 525。

### Step 2：删除插件系统与重型二进制 Trace 录制（Plugins、Observability、Commit）
- **改动范围**：
  - 删除：`server/src/plugin/`、`server/plugins/`、`server/src/cursor/services/observability/`、`server/src/store/cursor_traces.rs`、`server/src/cursor/services/commit_message.rs`、`apps/desktop/src/features/plugins/`、`server/tests/cursor_trace_queue.rs`。
  - 修改：`server/src/provider/router.rs`、`server/src/cursor/services/model_catalog.rs`、`server/src/api/cursor/handlers.rs`、`server/src/control/service.rs` 移除插件分支与 `ADAPTER_ID_PREFIX`；`server/src/cursor/transport/{handle,registry}.rs` 与 `run_sse.rs` 移除 `CursorTraceRecorder`；`server/src/provider/recorder.rs` 与 `store/llm_calls.rs` 去掉 `llm_call_requests` 和 `llm_call_response_chunks` 写入；`server/src/local_app/proxy.rs` 从 `is_local_path` 移除 `WriteGitCommitMessage`。
- **验收命令与预期结果**：
  ```bash
  cargo test --workspace --all-targets
  ```
  - 预期：编译与全量测试通过；启动桌面包后 `~/.cursor-byok-v3/plugins` 不再被扫描，Cursor `AvailableModels` 正常注入 14 个配置模型，Agent 对话与工具调用正常。

### Step 3：引入 TOML 配置文件与 14 模型只读导出工具
- **改动范围**：
  - 新增：`server/src/config/mod.rs`、`server/src/config/schema.rs`、`server/src/config/export_db.rs`（实现 `~/.cursor-byok-lite/config.toml` 解析、`env:VAR` 凭据解析、保持与原 `model_hash` 一致的哈希计算、文件修改监控或显式重载，以及只读 `mode=ro&immutable=1` 从 `~/.cursor-byok-v3/cursor-byok.db` 导出 14 模型与端口配置的 CLI/工具函数）。
  - 修改：`server/Cargo.toml` 增加 `toml = "0.8"`；`model_catalog.rs`、`ProviderRouter`、`handlers.rs` 优先从 `ConfigStore` 读取模型列表与 `default_model`（`GetDefaultModel` / `GetDefaultModelForCli` 返回 `default_model` 对应 `model_hash`）。
  - 初次数据目录初始化：若 `~/.cursor-byok-lite/ca` 不存在且 `~/.cursor-byok-v3/ca` 存在，自动只读拷贝 `ca/`（权限 `0700`/`0600`），避免重复弹窗信任证书。
- **验收命令与预期结果**：
  ```bash
  cargo run -p cursor-server --bin byok-export -- --from ~/.cursor-byok-v3/cursor-byok.db --out ~/.cursor-byok-lite/config.toml
  cargo test --workspace --all-targets
  ```
  - 预期：原 `cursor-byok.db` mtime 与校验和未发生任何改变；生成的 `config.toml` 包含完整 14 个模型且 `model_hash` 与原库 100% 一致；在 Cursor 中切换多个模型均可正常流式回复。

### Step 4：替换存储层——内存 LRU 会话态 + `context_sync` 提速 + 轻量 `usage.db` 与 CSV 导出
- **改动范围**：
  - 新增：`server/src/store/memory.rs`（`MemoryStore`：256 MB / 10,000 条有界 LRU `blobs` 缓存 + 内存活跃会话状态、checkpoint lineage、tool_rounds、`context_usage_anchor`）、`server/src/store/usage.rs`（`UsageStore`：单表 `usage_records`、`today_and_month_summary()`、`prune(retention_days, max_rows)` + `PRAGMA incremental_vacuum`、`export_csv(path)`）。
  - 删除：`server/src/store/{cas,checkpoints,conversations,input_anchors,legacy_config,messages,migrations,models,overview,runs,settings,tool_rounds,storage}.rs` 及 `server/migrations/`。
  - 修改：`server/src/cursor/services/context_sync.rs` 改为**直接以 Cursor 下发的原始 `RequestContextPartReferences` 4 个 `blob_id` 为键做内存 LRU 缓存**，不再经 `prost` 重编码哈希反推且不写磁盘；`compile/run.rs`、`run/engine.rs`、`run/compaction.rs`、`checkpoint/` 改接 `MemoryStore`；`provider/recorder.rs` 改为写入 `UsageStore`（仅统计字段）。
- **验收命令与预期结果**：
  ```bash
  cargo test --workspace --all-targets
  ```
  - 预期：长对话（含 `/summarize` 及触发上下文预算自动压缩）与多轮工具调用、打断恢复全通过；连续两次新对话命中 `context_sync` 内存缓存且本地准备耗时 `< 15 ms`；实测 `~/.cursor-byok-lite/usage.db` 体积 `< 1 MB`，导出 CSV 格式正确。

### Step 5：拆除 Tauri 与 Web 前端，切换为纯 Rust 原生 macOS 菜单栏应用
- **改动范围**：
  - 删除：`apps/desktop/` 全目录及 `server/src/control/`（Web Console 路由与 API 适配器）。
  - 新增：`apps/menubar/`（`Cargo.toml`、`Info.plist`、黑白模板图标 `trayTemplate.png`、`src/{main,tray,actions,resource_limits}.rs`），使用 `tray-icon` + `muda` + `tao`（`ActivationPolicy::Accessory`）与 `flock` 单实例锁：
    - 菜单结构：
      1. 状态只读项：`状态: 接管中 (51866 / 52148)` / `状态: 已暂停`
      2. 当前模型：`默认模型: deepseek-v4.1-flash`
      3. 用量汇总：`今日 Token: X | 本月: Y`
      4. 最近错误（仅存在时显示）：`最近错误: <截断摘要>`（点击可拷贝或打开日志）
      5. 分隔线
      6. `开启接管` / `暂停接管`（异步调用 `CursorHarness::enable/disable`）
      7. `切换默认模型 ▸`（子菜单列出 `config.toml` 全部启用模型，勾选当前 `default_model`，切换后即时更新内存并回写 `config.toml`）
      8. `重载配置`（重新读取 `config.toml`）
      9. `导出用量 CSV…`（调用 `rfd::FileDialog` 或保存至 `exports/` 并 `open -R`）
      10. `打开配置文件`（`open ~/.cursor-byok-lite/config.toml`）
      11. `打开日志目录`（`open ~/.cursor-byok-lite/logs`）
      12. 分隔线 + `退出`（触发 `harness.disable()` 清理 `settings.json` 代理并优雅停服）
  - 修改：`Cargo.toml` 将 member 改为 `apps/menubar`；`Makefile` 增加 `build-app` 目标生成 arm64 `Cursor BYOK Lite.app`。
- **验收命令与预期结果**：
  ```bash
  make build-app
  open "target/release/bundle/Cursor BYOK Lite.app"
  ps -o pid,rss,comm -p $(pgrep -x cursor-byok-lite)
  ```
  - 预期：无 Dock 图标、无 WebKit 子进程；菜单栏图标显示与全部菜单动作可用；开关接管正确读写 Cursor `settings.json`；Cursor 3.31.3 端到端 Agent 对话、工具调用、多模型切换全正常。

### Step 6：依赖树大扫除与全量量化指标验收
- **改动范围**：
  - 清理 `Cargo.toml` 与 `server/Cargo.toml` 中残留未用依赖（`serde_yaml`、`zip`、`include_dir`、`json5` 仅 `local_app/settings.rs` 需保留、`rayon`/`bincode`/`half` 等已随 `semble-core` 移除）；确认 `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --check` 零警告。
  - 若 `usage.db` 的 `sqlx` 编译宏/依赖占比可接受则保留最小 `sqlx` 单表（遵循 `AGENTS.md` 优先复用现有依赖），否则评估精简 `sqlx` feature。
- **验收命令与预期结果**：
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace --all-targets
  du -sh "target/release/bundle/Cursor BYOK Lite.app"
  ```
  - 预期对照第 3.6 节量化指标逐项达标：`.app < 20 MB`、常驻 RSS `< 25 MB`、冷启动 `< 0.4 s`、首 token 本地延迟 `< 15 ms`、`usage.db < 30 MB`。

---

## 5. 风险清单与快速回退方案

### 5.1 风险点与缓释措施

| 风险场景 | 触发机制 | 缓释与处置方案 |
| :--- | :--- | :--- |
| **内存 LRU 淘汰后跨轮读取旧 Blob 失败** | 长对话或挂起较久的会话超出 256 MB 内存缓存后继续追问。 | `BlobSynchronizer::get` 已有通过 `KvServerMessage::GetBlobArgs` 向 Cursor 客户端反查的能力；确保 `MemoryBlobCache` 未命中时不报错直接走 `GetBlobArgs` 回源即可。 |
| **进程异常退出残留 `settings.json` 代理导致 Cursor 断网** | 强杀 `cursor-byok-lite`（`kill -9`）跳过 `harness.disable()`。 | 启动阶段继续保留 `harness.cleanup_stale_settings()`；若启动时 `takeover_enabled = true` 则立即拉起 `52148` 代理恢复连通。 |
| **首次开启接管时触发 `pkill -x Cursor`** | `local_app/process.rs` 在首次写入 `settings.json` 代理键时强关 Cursor 以使其重读代理配置。 | 属于既有预期一次性行为；若此前已与原版共用了 `52148` 端口且配置未变，建议在切换时先关闭旧助手、保留 `settings.json` 或在菜单提示用户保存 Cursor 文件后再开启接管。 |
| **CA 证书路径隔离导致 TLS 不受信任** | 若 `~/.cursor-byok-lite/ca` 重新生成新根证书，系统钥匙串不信任新 CA 会引发 Cursor `CONNECT` 失败。 | Step 3 明确约定：**首次初始化时从 `~/.cursor-byok-v3/ca` 只读复制到 `~/.cursor-byok-lite/ca`**，无缝复用已受信任的系统根证书；仅当旧目录无 CA 时才新建并提示安装。 |
| **TOML 手误编辑导致模型加载失败** | 用户手改 `config.toml` 出现语法错误。 | 启动/重载校验失败时**保留上一次有效配置继续服务**，在托盘菜单「最近错误」及日志标红提示行号与原因，避免进程崩溃或 Cursor 断连。 |
| **Cursor 自动升级变更 `BidiAppend` / `RunInference` 门控** | 本机 Cursor 处于正式版通道更新。 | 保留 `analytics.rs`（钉死 `nal_websocket_client=false`、`agent_host_local_loop=false`）、`RunInference` `permission_denied` 回退与 `data_binary` XOR 解码；保留 `support/cursor-{capture,protocol-extractor}` 供日后快速适配。 |

### 5.2 从轻量版一键回退至现有版本（< 30 秒恢复）

因为轻量版使用独立目录 `~/.cursor-byok-lite/`（`config.toml`、`usage.db`、`logs/`）及独立 `.app` 名称（`Cursor BYOK Lite.app`），**全程不修改 `~/.cursor-byok-v3/cursor-byok.db` 与 `/Applications/Cursor助手.app`**，回退只需 3 步：

```bash
# 1. 通过菜单栏「退出」停止轻量版，并确认 51866 / 52148 端口已释放
lsof -nP -iTCP:51866 -iTCP:52148 -sTCP:LISTEN || true

# 2. 启动原有版本（/Applications/Cursor助手.app 或阶段 1 构建产物）
open "/Applications/Cursor助手.app"
# 若需切回阶段 1 aarch64 构建：
# open "$HOME/Applications/cursor-byok-329-stage1/Cursor BYOK.app"

# 3. 确认旧助手重新写入 settings.json 并接管 51866 / 52148
lsof -nP -iTCP:51866 -iTCP:52148 -sTCP:LISTEN
grep '"http.proxy"' "$HOME/Library/Application Support/Cursor/User/settings.json"
```
