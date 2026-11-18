# cursor-byok 适配 Cursor 3.29.16+（含本机实装 3.31.3）实施方案

## 结论先行

1. **根因已 100% 证实（非猜测）**：Cursor 自 3.29 起在 `extensions/cursor-always-local/dist/main.js` 中默认开启 `BidiAppendRequest` 二进制编码（门控 `bidi_append_binary_encoding_disabled` 默认关闭），将 `AgentClientMessage.toBinary()` 的**裸 protobuf 字节**直接填入 `BidiAppendRequest.data_binary`（tag 4），不再 hex 编码写入 `data`（tag 1）。而 `server/src/api/cursor/bidi.rs:163` 一旦见到 `!request.data_binary.is_empty()` 立即返回 `400 Bad Request`，导致 `BidiAppend` 在建立 `TransportHandle` 前失败，并发的 `RunSSE` 一直挂在 `wait_route` 上，Cursor 前端表现为无限 `"Reconnecting..."`。
2. **本机版本重要事实**：本机 Cursor 在 `2026-11-09` 自动升至 `3.29.16` 触发故障；在 `2026-11-18 14:21` 重启时已被 Electron ShipIt 原地更新为 **`3.31.3`**（`commit 001dff599231fb3858423e5000458b2694c724c0`，日志 `14:28:45` 复现完全相同的 `data_binary` 400 报错）。本方案基于实装 `3.31.3` 提取并校验协议，**同时覆盖 3.29.16 与 3.31.3**。
3. **阶段 1 最小改动范围**：**无需重生成 `.proto`**（`protocols/cursor/aiserver_v1.proto:3526`、`server/build.rs:34`、`server/src/cursor/protocol/proto.rs:25-26` 均已定义并解码 `pub data_binary: Vec<u8>`）。只需修改 **`server/src/api/cursor/bidi.rs`** 的 `decode()` 函数（支持 `data` 与 `data_binary` 严格二选一 XOR 语义）并补充单元与集成测试（共改 **2 个文件**，净增约 **225 行**含测试，生产代码净改 **≈15 行**）。下行 `RunSSE` 与 `BidiPollResponse` 均无二进制字段变更。

---

## 1. 架构与主链路分析

> 行数说明：以下目录树中，现有文件标注实测物理总行数（`wc -l`）及不含测试（`#[cfg(test)]` 之前）的非空生产代码行数 `prod`；拟改文件标注目标估算行数 `≈N 行`。

### 1.1 当前相关目录结构 vs 阶段 1 拟议结构

**模块变更清单**：
- **移动 / 合并 / 拆分 / 删除的模块**：无（`0` 个）。
- **原地修改的文件（阶段 1）**：
  1. `server/src/api/cursor/bidi.rs`：`decode()` 改为 `data` XOR `data_binary` 解码，新增 `#[cfg(test)] mod tests` 单元测试。
  2. `server/tests/connect_wire.rs`：新增 `data_binary` 端到端 `BidiAppend → Provider → RunSSE` 与双字段冲突 `400` 集成测试。

```text
cursor-byok/                                    # [现有 → 阶段 1]
├── Makefile                                    # 43 行 (prod 32)；check / build-desktop 与本地签名密钥目标
├── protocols/cursor/
│   ├── agent_v1.proto                          # 14,689 行；build.rs 经 prost_build 全量编译（阶段 1 不动）
│   └── aiserver_v1.proto                       # 45,020 行；含 BidiAppendRequest.data_binary = 4（阶段 1 不动）
├── server/
│   ├── build.rs                                # 54 行 (prod 50)；编译 agent_v1.proto 并断言 aiserver Bidi 契约
│   ├── src/
│   │   ├── app.rs                              # 208 行 (prod 195)；组装 Store/Provider/TransportRegistry/CursorHarness 并监听 51866
│   │   ├── config.rs                           # 170 行 (prod 139)；管理 ~/.cursor-byok-v3 数据目录与端口持久化配置
│   │   ├── error.rs                            # 75 行 (prod 72)；Error::Protocol / Decode → HTTP 400 Bad Request
│   │   ├── api/cursor/
│   │   │   ├── mod.rs                          # 8 行 (prod 6)；导出 Cursor 路由
│   │   │   ├── handlers.rs                     # 392 行 (prod 381)；BidiAppend / RunSSE 入口路由与 BYOK vs 官方分流
│   │   │   ├── bidi.rs                         # 211 行 (prod 201) → ≈315 行 (prod ≈208，新增测试 ≈100 行)；[阶段 1 修改] BidiAppend 解码与入队
│   │   │   ├── run_sse.rs                      # 232 行 (prod 214)；订阅 OutputHub 并输出 Connect SSE 帧流
│   │   │   └── proxy.rs                        # 227 行 (prod 207)；非本地模型/非本地接口透明转发 https://api2.cursor.sh
│   │   ├── cursor/
│   │   │   ├── protocol/
│   │   │   │   ├── proto.rs                    # 107 行 (prod 97)；引入生成版 agent::v1 并手写 aiserver::v1 Bidi 子集（已含 data_binary）
│   │   │   │   ├── connect.rs                  # 122 行 (prod 107)；Connect 5 字节帧编解码与 decode_unary
│   │   │   │   └── events.rs                   # 172 行 (prod 157)；ModelEvent → AgentServerMessage 转换
│   │   │   ├── transport/
│   │   │   │   ├── registry.rs                 # 272 行 (prod 245)；request_id → LocalTransport / Upstream 路由表与 wait_route 同步原语
│   │   │   │   ├── handle.rs                   # 181 行 (prod 151)；单次 request_id 输入投递、OutputHub 订阅与生命周期门控
│   │   │   │   ├── inbox.rs                    # 30 行 (prod 26)；按 append_seqno 重排乱序 BidiAppend 请求
│   │   │   │   └── output.rs                   # 53 行 (prod 47)；Connect 帧历史缓存、重放、多订阅广播与原子关闭
│   │   │   └── services/
│   │   │       ├── server_config.rs            # 51 行 (prod 34)；返回 http2_config = FORCE_ALL_DISABLED(1)，强制 Cursor 走 HTTP/1.1 Bidi+SSE
│   │   │       └── analytics.rs                # 176 行 (prod 164)；拦截 BootstrapStatsig 注入 nal_agent_retries = true
│   │   ├── local_app/
│   │   │   ├── mod.rs                          # 235 行 (prod 211)；CursorHarness 接管状态机、CA 校验与启停控制
│   │   │   ├── proxy.rs                        # 222 行 (prod 180)；监听 127.0.0.1:52148 的 Hudsucker TLS MITM 代理（CursorRelay）
│   │   │   ├── settings.rs                     # 111 行 (prod 101)；读写 Cursor settings.json 中 http.proxy 等 5 个键
│   │   │   └── process.rs                      # 91 行 (prod 81)；首次写入代理配置前调用 pkill -x Cursor
│   │   └── control/
│   │       └── ads.rs                          # 408 行 (prod 311)；拉取 tab.leokun.cn 菜单推广位（525 告警来源）
│   └── tests/
│       └── connect_wire.rs                     # 140 行 → ≈265 行（新增测试 ≈125 行）；[阶段 1 修改] Connect 帧与 BidiAppend 线级测试
└── apps/desktop/
    ├── package.json                            # 64 行；前端与 tauri:build 脚本定义
    └── src-tauri/
        ├── tauri.conf.json                     # 45 行；identifier="dev.cursorbyok.desktop", productName="Cursor BYOK", version="1.0.1"
        └── src/
            ├── desktop.rs                      # 320 行 (prod 300)；Tauri 单实例锁、内嵌 cursor-server 启动、托盘与优雅退出
            └── startup.rs                      # 139 行 (prod 91)；初始化 ~/.cursor-byok-v3/logs 按 UTC 日期轮转日志
```

### 1.2 Cursor ↔ cursor-byok 完整主执行路径（数据流与控制流）

```text
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│ Cursor Desktop (3.29.16 / 3.31.3) — extensions/cursor-always-local/dist/main.js          │
│                                                                                          │
│ 1) 启动时请求 GetServerConfig ──► 收到 http2_config = 1 (FORCE_ALL_DISABLED)              │
│    ──► AiConnectTransportHandler 选择 HTTP/1.1 Bidi+SSE 传输 (vhr = "http1_sse")          │
│    （注：仅当 isHttp1SseDisabled() 为 true 时才走 Phr = BidiPoll/RunPoll，默认不走）       │
│                                                                                          │
│ 2) 发起对话 (requestId = UUID)：并行发出两条 HTTP/1.1 请求（经 http.proxy 127.0.0.1:52148）│
│    ├── [上行] POST https://api2.cursor.sh/aiserver.v1.BidiService/BidiAppend             │
│    │         Body = BidiAppendRequest { request_id, append_seqno, data_binary: B }       │
│    │         其中 B = AgentClientMessage.toBinary()（裸 protobuf，可带外层 HTTP gzip）    │
│    └── [下行] POST https://api2.cursor.sh/agent.v1.AgentService/RunSSE                   │
│              Body = ConnectFrame(BidiRequestId { request_id })                           │
└───────────────────┬──────────────────────────────────────────────────────▲───────────────┘
                    │ CONNECT api2.cursor.sh:443 (TLS MITM via ~/.cursor-byok-v3/ca)
                    ▼                                                      │
┌──────────────────────────────────────────────────────────────────────────┴───────────────┐
│ local_app/proxy.rs :: CursorRelay (127.0.0.1:52148)                                      │
│ ├── is_cursor_host(host) && should_route_locally(path)                                   │
│ │   ├── 本地接管路径（RunSSE / BidiAppend / AvailableModels / GetServerConfig 等 31 条）  │
│ │   │   ──► 注入 x-server-upstream-url 头，改写 URI 至 http://127.0.0.1:51866/<path>      │
│ │   └── 其他 *.cursor.sh 路径 ──► 透传官方上游                                            │
└───────────────────┬──────────────────────────────────────────────────────▲───────────────┘
                    │ HTTP 127.0.0.1:51866                                 │
                    ▼                                                      │
┌──────────────────────────────────────────────────────────────────────────┴───────────────┐
│ api/cursor/handlers.rs (cursor-server Axum Router)                                       │
│   RequestDecompressionLayer::new()（自动解压外层 Content-Encoding: gzip）                 │
│                                                                                          │
│   [上行分支: bidi_handler]                        [下行分支: run_sse_handler]             │
│   1. connect::decode_unary::<BidiAppendRequest>   1. connect::decode_unary::<BidiRequestId>
│   2. bidi::decode(&request)  ◄── ★ 故障点(400)     2. registry.wait_route(&request_id).await
│   3. 首包检查 decoded.model_id()：                   ├── TransportRoute::Local            │
│      ├── store.model(id).is_some() 或 plugin         │   ──► run_sse::stream()            │
│      │   ──► local = true                            │       └── handle.subscribe()       │
│      └── 官方模型 ──► registry.mark_upstream()       │           ──► OutputHub 历史重放   │
│                      + proxy::forward()              │               + 实时帧流           │
│   4. bidi::append(&registry, decoded, parent)        └── TransportRoute::Upstream(gen)   │
│      ├── registry.get_or_create_for_append()             ──► proxy::forward()             │
│      │   （唤醒 wait_route! 绑定 Conversation）                                           │
│      ├── ClientHeartbeat ──► handle.emit(heartbeat)                                      │
│      └── handle.command(Append { seqno, message })                                       │
│   5. 返回 200 OK (empty BidiAppendResponse)                                              │
└───────────────────┬──────────────────────────────────────────────────────▲───────────────┘
                    │ TransportCommand::Append                             │ Connect 5B Frames
                    ▼                                                      │ (0x00 data / 0x02 "{}")
┌──────────────────────────────────────────────────────────────────────────┴───────────────┐
│ cursor/conversation/ (ConversationRuntime: 单 conversation_id 唯一所有者)                 │
│ ├── OrderedInbox (按 append_seqno 严格顺序投递)                                           │
│ ├── compile/{run,action,context,model}.rs (AgentClientMessage → PreparedRun / Action)    │
│ ├── current_run: Option<RunHandle> ──► run::RunEngine                                    │
│ │   ├── ModelCycle ──► provider::ProviderRouter ──► OpenAiChat (deepseek-v4.1-flash)     │
│ │   └── ToolRound  ──► cursor/tools/ (ExecServerMessage / InteractionQuery ↔ BidiAppend) │
│ ├── conversation/output.rs + protocol/events.rs ──► AgentServerMessage ──► OutputHub ────┘
│ └── checkpoint/{builder,worker}.rs ──► KvServerMessage(SetBlob) & SQLite Store
│     (~/.cursor-byok-v3/cursor-byok.db: conversations / messages / checkpoints / runs)
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

### 1.3 并发时序与状态边界（修复前 vs 修复后）

`BidiAppend` 与 `RunSSE` 是由客户端并发建立、通过 `TransportRegistry` 内 `request_id` 汇合的双通道生命周期：

```text
修复前（1.0.1 遇到 data_binary）：
  Cursor                         bidi_handler                     TransportRegistry              run_sse_handler
    │── POST RunSSE(req_1) ─────────────────────────────────────────────────────────────────────────►│
    │                                                                     │◄── wait_route("req_1") ──│ (挂起等待路由判定)
    │── POST BidiAppend(req_1, seqno=0, data_binary=RunRequest) ──►│      │                          │
    │                                 bidi::decode() 拒绝!         │      │                          │
    │◄── 400 Bad Request ─────────────(未调用 get_or_create)       │      │                          │
    │                                                              │ (无 req_1 条目，route_changed 未触发)
    │── 客户端中止流 / 重试新 req_2 ───────────────────────────────┼────────────────────────────────►│ 连接断开，UI 显示
                                                                                                       "Reconnecting..."

修复后（阶段 1）：
  Cursor                         bidi_handler                     TransportRegistry              run_sse_handler
    │── POST RunSSE(req_1) ─────────────────────────────────────────────────────────────────────────►│
    │                                                                     │◄── wait_route("req_1") ──│
    │── POST BidiAppend(req_1, seqno=0, data_binary=RunRequest) ──►│      │                          │
    │                                 bidi::decode() 成功          │      │                          │
    │                                 store.model("83f1333d63840a0c") 命中│                          │
    │                                 get_or_create_for_append ───►│ 插入 local["req_1"]             │
    │                                                              │── notify_waiters() ────────────►│ 返回 TransportRoute::Local
    │◄── 200 OK (application/proto) ───────────────────────────────│                                 │── 200 OK (text/event-stream)
    │◄══ Connect Frames (KvServerMessage / TextDelta / TurnEnded / END_STREAM 0x02 "{}") ════════════│
```

- **同生命周期延续 vs 新生命周期创建**：
  - 同一 `request_id` 下的后续 `seqno = 1..N`（如 `RequestContextResult`、`KvClientMessage`、`ExecClientMessage`、`ClientHeartbeat`、运行中插入消息）均不含 `model_id`，`handlers.rs:259` 命中 `registry.local(&request_id).is_some()`，复用同一个 `TransportHandle`（`replace_closing = false`）。
  - 每一轮新 Agent Run 由 Cursor 生成全新的 `request_id`（或首包携带 `model_id` 时 `replace_closing = true`），创建新的 `LocalTransport (generation + 1)`，并在 `ConversationRegistry` 中绑定到同一个持久化 `conversation_id`。

---

## 2. Cursor 协议提取、Diff 与 `data_binary` 实证

### 2.1 提取与对比命令

```bash
# 1. 从本机 /Applications/Cursor.app 提取全量 proto（注意：3.29+/3.31 多包合并存在重复 tag 及新增紧凑描述符，
#    support/cursor-protocol-extractor/scripts/extract.sh 默认带 -strict 会以退出码 1 报错拦截，故验证提取不加 -strict）
(
  cd support/cursor-protocol-extractor
  INPUT_ARGS=()
  while IFS= read -r js; do
    INPUT_ARGS+=( -input "$js" )
  done < <(find /Applications/Cursor.app -type f ! -path "*/node_modules/*" \
    \( -name "workbench.desktop.main.js" -o -name "extensionHostProcess.js" -o -path "*/extensions/*/dist/main.js" \) | sort)
  go run ./extractor "${INPUT_ARGS[@]}" -output ../../.scratch/cursor-329/extracted-329 -skip-format
)

# 2. 结构化对比 protocols/cursor/*.proto 与 .scratch/cursor-329/extracted-329/*.proto
python3 .scratch/cursor-329/diff_protos.py > .scratch/cursor-329/proto_diff_report.txt

# 3. 针对 cursor-byok 接管链路与 server/src Rust 引用的专项交叉检查
python3 .scratch/cursor-329/analyze_takeover_diff.py
```

### 2.2 全量文件级 Diff 概览

| Proto 文件 | 消息数（旧 → 新） | 枚举数（旧 → 新） | 服务数（旧 → 新） | 与 `cursor-byok` 编译/运行时的关系 |
| :--- | :--- | :--- | :--- | :--- |
| `agent_v1.proto` | `1072 → 1145` (`+272 / -199`) | `67 → 95` (`+42 / -14`) | `9 → 1` (`+0 / -8`) | `server/build.rs` 唯一 `prost_build` 编译输入 |
| `aiserver_v1.proto` | `5215 → 6684` (`+2305 / -836`) | `455 → 792` (`+403 / -66`) | `63 → 39` (`+7 / -31`) | `server/build.rs` 文本契约断言；`proto.rs` 手写子集 |
| `anyrun_v1.proto` | `157 → 34` (`+1 / -124`) | `28 → 11` (`+2 / -19`) | `0 → 0` | 未被服务端编译或引用 |
| `git_forge_v1.proto` | `115 → 62` (`+12 / -65`) | `8 → 13` (`+6 / -1`) | `0 → 0` | 未被服务端编译或引用 |
| `internapi_v1.proto` | `3 → 3` (`+0 / -0`) | `1 → 1` (`+0 / -0`) | `0 → 0` | 完全未变；未被服务端编译或引用 |
| `origin_v1.proto` | `524 → 1116` (`+645 / -53`) | `40 → 123` (`+84 / -1`) | `1 → 1` | 未被服务端编译或引用 |
| `scmapi_v0.proto` | `0 → 85`（新增文件） | `0 → 10` | `0 → 1` | 新增 SCM 服务包，不影响接管链路 |

> **提取器差异说明**：Cursor 3.29+/3.31 在 `extensions/cursor-always-local/dist/main.js` 与 `extensions/cursor-local-agent-runtime/dist/main.js` 中将部分 `@bufbuild/protobuf` 类型升级为紧凑字符串描述符 `static $(){return["TypeName|1 field_name type_id|..."]}`。现有 `support/cursor-protocol-extractor` 仅解析 `makeMessageType`，导致部分实际仍存在的消息（如 `BidiAppendRequest` 在 `BidiService` 中显示为混淆类名 `JE`、`ConversationSummary` 未提取）在原始 diff 中呈现为“删除”。我们已通过 `.scratch/cursor-329/inspect_cursor_js.py` 直接核对了 JS 源码中的 `static $()` 描述符。

### 2.3 影响接管链路的所有协议变化清单

| 链路位置 / 服务 | 消息 / RPC | 变化详情 | 响应方向是否有二进制变化 | 对接管链路的影响与处理 |
| :--- | :--- | :--- | :--- | :--- |
| **上行主通道**<br>`aiserver.v1.BidiService/BidiAppend` | `BidiAppendRequest`<br>(`1: string data`, `2: BidiRequestId request_id`, `3: int64 append_seqno`, `4: bytes data_binary`) | Schema 字段与 tag **完全未变**；客户端默认从填 `data`（hex）切换为填 `data_binary`（裸 bytes），受 Statsig 开关 `bidi_append_binary_encoding_disabled` 控制。 | `BidiAppendResponse` 仍为空消息 `{}` | **【阶段 1 唯一阻塞项】** 修改 `bidi::decode()` 同时支持 `data_binary` 与 `data`（严格 XOR）。 |
| **下行主通道（SSE）**<br>`agent.v1.AgentService/RunSSE` | 入参 `BidiRequestId`；出参 `stream AgentServerMessage` | RPC 签名、Connect 5 字节流式帧封装、`END_STREAM_FLAG (0x02)` 结束帧 `{}` **完全未变**。 | **无变化**（本来就是 Connect 帧内裸 `AgentServerMessage` protobuf） | 无需修改。 |
| **下行轮询备选通道**<br>`agent.v1.AgentService/RunPoll` & `BidiPoll*` | `BidiPollRequest` / `BidiPollResponse` | `BidiPollResponse` 描述符仍为 `"BidiPollResponse\|1 seqno 3\|2 data 9\|3 eof 8?"`，客户端仍用 `Buffer.from(r.data, "hex")` 解码。 | **无新增二进制字段**（仍只有 `string data = 2`） | 默认 `isHttp1SseDisabled() == false` 不走 Poll；无需修改。 |
| **新增可选传输通道**<br>Agent WebSocket (`cursor-always-local`) | `wss://api2.cursor.sh/agent/v1/run` | 客户端新增 `AgentRunWebSocketSelection`（受 Statsig 门控 `nal_websocket_client` 控制，默认关闭；若开启且握手被拒则记忆 `upgradeRefused = true` 回退 HTTP）。 | WebSocket 帧内为 `AgentServerMessage` 二进制 | 当前未开启，本机实测仍走 `BidiAppend` + `RunSSE`；阶段 2 可在 `BootstrapStatsig` 显式钉死 `nal_websocket_client = false`。 |
| **上行内层消息**<br>`agent.v1.AgentClientMessage` | `oneof message` (`1..8`：`RunRequest` / `ExecClientMessage` / `ExecClientControlMessage` / `KvClientMessage` / `ConversationAction` / `InteractionResponse` / `ClientHeartbeat` / `PrewarmRequest`) | 顶层 `oneof` 字段号 `1..8` **完全未变**。`AgentRunRequest` 删除未使用的 `10, 27`，新增可选字段 `28..37`（`system_prompt_spec`、`client_llm_gateway_credential`、`emit_model_call_usage` 等）。 | 不适用（上行） | `prost` 自动跳过未知可选字段；现有模型提取、会话恢复、动作编译完全兼容。 |
| **会话动作与历史**<br>`agent.v1.ConversationAction` | `UserMessageAction` / `UserMessage` / `BackgroundTaskCompletionAction` / `AsyncAskQuestionCompletionAction` / `RequestContext` | `ConversationAction` 19 个分支未变；`UserMessageAction` 新增 `optional ConversationHistory trailing_conversation_history = 8`（客户端 JS 检索调用点为 `0`）；`BackgroundTaskCompletionAction` 新增 `system_reminder = 2, request_context = 3`；`RequestContext` 新增 `system_prompt_override = 53`。 | 不适用（上行） | 非阻塞新增可选字段；阶段 1 无需改动，阶段 2 持续观察 `trailing_conversation_history`。 |
| **下行内层事件**<br>`agent.v1.AgentServerMessage` | `InteractionUpdate` / `TurnEndedUpdate` / `ThinkingCompletedUpdate` / `SummaryCompletedUpdate` | `AgentServerMessage` 顶层未变。`InteractionUpdate` 删除 BYOK 从未发送的 `prompt_suggestion = 18`，新增 `routed_model = 24`、`message_started_at_ms = 25`、`model_call_completed = 31` 等可选分支；`TurnEndedUpdate` 新增 `optional uint64 ended_at_ms = 6`、`optional EstimatedCost estimated_cost = 7`。 | 无二进制包装变更 | 新增字段均为 `optional` 或未触发的新 `oneof` 分支，Cursor 对现有下行事件完全兼容。 |
| **工具调用与执行协议**<br>`ToolCall` / `ExecServerMessage` / `ExecClientMessage` | `Read` / `Edit` / `Delete` / `Grep` / `Ls` / `Shell` / `Mcp` 等 | BYOK 使用的全部工具 tag **100% 未变**；删除了未使用的实验性 `pi_*` 工具分支；新增云端/画布/视频/任务追踪工具分支（`73..95`）及 `ShellArgs`/`ReadArgs`/`McpArgs` 可选字段。 | 无变化 | 现有本地/Exec/MCP 工具链路完全兼容。 |
| **检查点状态**<br>`ConversationStateStructure` | `ConversationStateStructure` / `AgentConversationTurnStructure` / `ConversationTokenDetails` | 新增语音、线程回复、遥测等可选字段（`34..55`）；`ConversationTokenDetails` 移除未使用的 `prompt_context_usage_snapshot_blob_id = 5`（保留 `used_tokens=1, max_tokens=2, breakdown=3`）。 | 无变化 | BYOK 读写的 tag 全部保持不变。 |
| **外围接管接口**<br>`AgentService` / `AiService` / `ServerConfigService` / `DashboardService` / `AuthService` / `NetworkService` / `AnalyticsService` | `AvailableModels` / `GetUsableModels` / `GetServerConfig` / `BootstrapStatsig` / `IsConnected` / `WriteGitCommitMessage` / `ErrorDetails` | `AvailableModelsResponse.AvailableModel` 仅尾部新增可选字段 `49..51`；`GetServerConfigResponse` 保留 `config_version=6, http2_config=7, cli_sandbox_default_enabled=28`；`ErrorDetails`、`CustomErrorDetails`、`BootstrapStatsigResponse`、`WriteGitCommitMessage*`、`IsConnected` **完全未变**。`AgentService` 新增 `WarmUp` 等 4 个非核心 RPC（未拦截时走 `proxy::forward`）。 | 无变化 | 本机 `2026-11-18` 日志已证实 `AvailableModels`（注入 14 个 BYOK 模型）、`GetEffectiveUserPlugins`、`GetMe`、`UpdateConversationMetadata` 等均返回 `200 OK`。 |

### 2.4 `data_binary` 格式验证结论（已验证）

- **结论**：**已验证（Confirmed）**——`BidiAppendRequest.data_binary` 的内容就是**裸的 `agent.v1.AgentClientMessage` protobuf 序列化字节**，无 hex 编码、无内层 Connect 5 字节帧头、无字段内单独压缩。
- **直接证据链**：
  1. **Cursor 实装客户端源码（`/Applications/Cursor.app/Contents/Resources/app/extensions/cursor-always-local/dist/main.js`）**：
     - 消息描述符（偏移 `1079074`）：
       ```js
       static $() { return ["BidiAppendRequest|1 data 9|2 request_id #0|3 append_seqno 3|4 data_binary 12", SE] }
       // proto3 类型码：9 = string (data), #0 = BidiRequestId (request_id), 3 = int64 (append_seqno), 12 = bytes (data_binary)
       ```
     - 默认门控（偏移 `3980188` & `3994700`）：
       ```js
       this.bidiAppendBinaryEncodingDisabled = new Ohr(t, "bidi_append_binary_encoding_disabled");
       const i = () => !this.bidiAppendBinaryEncodingDisabled.isOn; // 默认 true
       ```
     - 上行构造逻辑（偏移 `3894567`）：
       ```js
       const J = new SE({ requestId: o }),
             B = w.value.toBinary(), // w.value 即 AgentClientMessage 实例
             q = l
               ? { requestId: J, appendSeqno: BigInt(S), dataBinary: B }
               : { requestId: J, appendSeqno: BigInt(S), data: Buffer.from(B).toString("hex") };
       await this.bidiClient.bidiAppend(q, { headers: s });
       ```
       可见新路径 `dataBinary: B` 与旧路径 `Buffer.from(B).toString("hex")` 共享同一个 `B = w.value.toBinary()`，唯一区别是省去了 hex 编解码。
  2. **仓库内抓包器印证（`support/cursor-capture/decode.go:216-223`）**：抓包工具早已按 `proto.Unmarshal(request.GetDataBinary(), &agentv1.AgentClientMessage{})` 直接反序列化裸字节。
  3. **历史成功日志对照（`~/.cursor-byok-v3/logs/cursor-byok.2026-11-02.log` ～ `11-03.log`）**：升级前使用 `deepseek-v4.1-flash`（`model_hash = "83f1333d63840a0c"`）时，外层 HTTP 请求经 `handlers.rs:160` 的 `RequestDecompressionLayer` 解压与 `connect::decode_unary` 解包后进入 `bidi::decode()`，产出 `DecodedAppend` 并打印 `routing Cursor Run to BYOK provider request_id="..." model_id="83f1333d63840a0c"`。

---

## 3. 实施方案（分阶段）

### 阶段 1：修到能用（最小改动 + 原生 arm64 桌面包构建 + 端到端验收）

#### 3.1.1 Proto 是否需要重新生成？

**不需要，且阶段 1 严禁覆盖 `protocols/cursor/*.proto`。** 原因有二：
1. `protocols/cursor/aiserver_v1.proto:3526`、`server/build.rs:34` 以及 `server/src/cursor/protocol/proto.rs:25-26` **已经包含** `#[prost(bytes = "vec", tag = "4")] pub data_binary: Vec<u8>`，`prost` 当前已将线上的 `data_binary` 完整解码到结构体字段中。
2. 如第 2.2 节所述，未经改造提取器就直接用提取产物覆盖 `protocols/cursor/agent_v1.proto` 会因 `ListAgentHostSessionsRequest` 重复 tag `1` 及 `ConversationSummary` 未从 `static $()` 提取而直接导致 `cargo build` 编译失败。

#### 3.1.2 具体代码修改

##### 文件 1：`server/src/api/cursor/bidi.rs`

- **修改函数**：`pub fn decode(request: &ai::BidiAppendRequest) -> Result<DecodedAppend>`（第 `156–180` 行）。
- **`data` 与 `data_binary` 语义规范**：
  - **严格二选一（XOR）**：
    - `data` 为空、`data_binary` 非空（Cursor 3.29+ 默认形态）：直接对 `request.data_binary.as_slice()` 执行 `agent::AgentClientMessage::decode`（零拷贝、零额外分配）。
    - `data` 非空、`data_binary` 为空（Statsig 回退开关开启或旧版 CLI 形态）：`hex::decode(&request.data)` 后执行 `agent::AgentClientMessage::decode`。
    - **两者同时为空**：返回 `Err(Error::Protocol("BidiAppend contains no AgentClientMessage".into()))`（保持现有 `400` 错误文案，兼容 `connect_wire.rs:138` 既有断言）。
    - **两者同时非空**：显式拒绝并返回 `Err(Error::Protocol("BidiAppend cannot set both data and data_binary".into()))`（映射为 `400 Bad Request`；遵循 `AGENTS.md` 不做含糊猜测或静默回退）。

将 `server/src/api/cursor/bidi.rs` 第 `156–180` 行替换为：

```rust
pub fn decode(request: &ai::BidiAppendRequest) -> Result<DecodedAppend> {
    let request_id = request
        .request_id
        .as_ref()
        .map(|id| id.request_id.as_str())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Protocol("BidiAppend request_id is required".into()))?;
    let message = match (request.data.is_empty(), request.data_binary.is_empty()) {
        (true, false) => agent::AgentClientMessage::decode(request.data_binary.as_slice())?,
        (false, true) => {
            let payload = hex::decode(&request.data)
                .map_err(|error| Error::Protocol(format!("invalid BidiAppend hex: {error}")))?;
            agent::AgentClientMessage::decode(payload.as_slice())?
        }
        (true, true) => {
            return Err(Error::Protocol(
                "BidiAppend contains no AgentClientMessage".into(),
            ));
        }
        (false, false) => {
            return Err(Error::Protocol(
                "BidiAppend cannot set both data and data_binary".into(),
            ));
        }
    };
    Ok(DecodedAppend {
        request_id: request_id.into(),
        seqno: request.append_seqno,
        message,
    })
}
```

- **同文件末尾追加单元测试模块 `#[cfg(test)] mod tests`**（覆盖 Cursor 3.29+/3.31 真实线级二进制封装样本、hex 形态、双空、双非空冲突、损坏 protobuf、缺失 `request_id` 共 6 个用例）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Cursor 3.29+/3.31 `cursor-always-local` 发送的真实线级形态：
    /// BidiAppendRequest { request_id: "req-329", append_seqno: 0, data_binary: AgentClientMessage.toBinary() }
    #[test]
    fn decode_accepts_cursor_329_data_binary_run_request_wire_bytes() {
        let client_msg = agent::AgentClientMessage {
            message: Some(agent::agent_client_message::Message::RunRequest(
                agent::AgentRunRequest {
                    conversation_id: Some("conv-329".into()),
                    requested_model: Some(agent::RequestedModel {
                        model_id: "83f1333d63840a0c".into(),
                        ..Default::default()
                    }),
                    action: Some(agent::ConversationAction {
                        action: Some(agent::conversation_action::Action::UserMessageAction(
                            agent::UserMessageAction {
                                user_message: Some(agent::UserMessage {
                                    text: "ping".into(),
                                    message_id: "msg-1".into(),
                                    mode: agent::AgentMode::Agent as i32,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
        };
        let wire = ai::BidiAppendRequest {
            data: String::new(),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-329".into(),
            }),
            append_seqno: 0,
            data_binary: client_msg.encode_to_vec(),
        }
        .encode_to_vec();
        // 校验 tag 4 (0x22 = field 4, wire type 2) 确实出现在外层 protobuf 字节流中
        assert!(wire.windows(2).any(|w| w[0] == 0x22));

        let raw_request = ai::BidiAppendRequest::decode(wire.as_slice()).unwrap();
        let decoded = decode(&raw_request).unwrap();
        assert_eq!(decoded.request_id, "req-329");
        assert_eq!(decoded.seqno, 0);
        assert_eq!(decoded.conversation_id(), Some("conv-329"));
        assert_eq!(decoded.model_id(), Some("83f1333d63840a0c"));
    }

    #[test]
    fn decode_accepts_hex_data_when_data_binary_is_empty() {
        // 真实捕获样本：KvClientMessage { id: 1, SetBlobResult {} } = 0x1a0408011a00
        let request = ai::BidiAppendRequest {
            data: "1a0408011a00".into(),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-hex".into(),
            }),
            append_seqno: 2,
            data_binary: Vec::new(),
        };
        let decoded = decode(&request).unwrap();
        assert_eq!(decoded.request_id, "req-hex");
        assert_eq!(decoded.seqno, 2);
        assert!(matches!(
            decoded.message.message,
            Some(agent::agent_client_message::Message::KvClientMessage(_))
        ));
    }

    #[test]
    fn decode_rejects_simultaneous_data_and_data_binary() {
        let payload = [0x1a, 0x04, 0x08, 0x01, 0x1a, 0x00];
        let request = ai::BidiAppendRequest {
            data: hex::encode(payload),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-both".into(),
            }),
            append_seqno: 1,
            data_binary: payload.to_vec(),
        };
        let error = decode(&request).err().expect("must reject dual payload");
        assert!(error
            .to_string()
            .contains("BidiAppend cannot set both data and data_binary"));
    }

    #[test]
    fn decode_rejects_empty_data_and_data_binary() {
        let request = ai::BidiAppendRequest {
            data: String::new(),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-empty".into(),
            }),
            append_seqno: 0,
            data_binary: Vec::new(),
        };
        let error = decode(&request).err().expect("must reject empty payload");
        assert!(error
            .to_string()
            .contains("BidiAppend contains no AgentClientMessage"));
    }

    #[test]
    fn decode_rejects_malformed_data_binary_protobuf() {
        let request = ai::BidiAppendRequest {
            data: String::new(),
            request_id: Some(ai::BidiRequestId {
                request_id: "req-bad-pb".into(),
            }),
            append_seqno: 0,
            data_binary: vec![0x0a, 0x0f, 0x01], // 截断的 length-delimited 字段
        };
        let error = decode(&request).err().expect("must reject truncated protobuf");
        assert!(matches!(error, Error::Decode(_)));
    }

    #[test]
    fn decode_rejects_missing_request_id() {
        let request = ai::BidiAppendRequest {
            data: String::new(),
            request_id: None,
            append_seqno: 0,
            data_binary: vec![0x1a, 0x04, 0x08, 0x01, 0x1a, 0x00],
        };
        let error = decode(&request).err().expect("must require request_id");
        assert!(error
            .to_string()
            .contains("BidiAppend request_id is required"));
    }
}
```

##### 文件 2：`server/tests/connect_wire.rs`

在 `server/tests/connect_wire.rs` 补充 2 个 HTTP 路由级集成测试：
1. `bidi_append_data_binary_routes_to_local_provider_and_streams_run_sse`：在 `temp_store()` 中写入自定义模型配置，通过 `POST /aiserver.v1.BidiService/BidiAppend` 发送仅含 `data_binary` 的 `RunRequest`（含 `RequestContext::default()`）并在后台读取 `POST /agent.v1.AgentService/RunSSE`，遇到 `KvServerMessage` 时继续用 `data_binary` 回传 `SetBlobResult` ack，验证 `FakeProvider` 输出的文本增量（`TextDeltaUpdate`）、`TurnEndedUpdate` 及 Connect 结束帧 `[2, 0, 0, 0, 2, b'{', b'}']` 完整流回客户端且全程 `200 OK`。
2. `bidi_append_rejects_simultaneous_data_and_data_binary_with_bad_request`：通过 HTTP 发送同时填充 `data` 和 `data_binary` 的请求，断言响应状态码为 `400 Bad Request` 且 JSON 错误消息包含 `"BidiAppend cannot set both data and data_binary"`。

```rust
#[tokio::test]
async fn bidi_append_rejects_simultaneous_data_and_data_binary_with_bad_request() {
    let (_directory, store) = fixtures::temp_store().await;
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let clients = NetworkClients::new(store.clone());
    let registry = TransportRegistry::new(
        store,
        Arc::new(fake_provider::FakeProvider::default()),
        PromptCompiler::new(assets),
    );
    let payload = [0x1a, 0x04, 0x08, 0x01, 0x1a, 0x00];
    let wire = ai::BidiAppendRequest {
        data: hex::encode(payload),
        request_id: Some(ai::BidiRequestId {
            request_id: "dual-payload".into(),
        }),
        append_seqno: 0,
        data_binary: payload.to_vec(),
    }
    .encode_to_vec();

    let response = cursor::router(registry, clients)
        .unwrap()
        .oneshot(
            Request::post("/aiserver.v1.BidiService/BidiAppend")
                .header(header::CONTENT_TYPE, "application/proto")
                .body(Body::from(wire))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("BidiAppend cannot set both data and data_binary"));
}

#[tokio::test]
async fn bidi_append_data_binary_routes_to_local_provider_and_streams_run_sse() {
    use cursor_server::{
        model::{ModelConfigInput, ModelType, OPENAI_CHAT_ENDPOINT},
        provider::{FinishReason, ModelEvent},
    };
    use futures_util::StreamExt;

    let (_directory, store) = fixtures::temp_store().await;
    let saved_model = store
        .create_model(&ModelConfigInput {
            sort_order: 1,
            display_name: "deepseek-v4.1-flash".into(),
            group_name: None,
            model_type: ModelType::OpenAi,
            base_url: "https://example.com/v1".into(),
            use_full_url: false,
            api_key: "test-key".into(),
            tooltip_data: "test".into(),
            model_id: "deepseek-v4.1-flash".into(),
            reasoning_effort: None,
            openai_endpoint: OPENAI_CHAT_ENDPOINT.into(),
            openai_extra_params_enabled: false,
            openai_extra_params: serde_json::json!({}),
            custom_headers_enabled: false,
            custom_headers: serde_json::json!({}),
            anthropic_extra_params_enabled: false,
            anthropic_extra_params: serde_json::json!({}),
            context_window_tokens: None,
            max_completion_tokens: None,
            anthropic_max_tokens: None,
            anthropic_thinking_effort: None,
            thinking_budget_tokens: None,
        })
        .await
        .unwrap();

    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "call-1".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("pong from byok".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let clients = NetworkClients::new(store.clone());
    let registry = TransportRegistry::new(store, Arc::new(provider), PromptCompiler::new(assets));
    let app = cursor::router(registry, clients).unwrap();

    let request_id = "req-data-binary-e2e";
    let run_sse_app = app.clone();
    let sse_task = tokio::spawn(async move {
        let req_frame = connect::encode_message(&pb::BidiRequestId {
            request_id: request_id.into(),
        })
        .unwrap();
        run_sse_app
            .oneshot(
                Request::post("/agent.v1.AgentService/RunSSE")
                    .header(header::CONTENT_TYPE, "application/connect+proto")
                    .body(Body::from(req_frame))
                    .unwrap(),
            )
            .await
            .unwrap()
    });

    let run_client_msg = pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                conversation_id: Some("conv-data-binary-e2e".into()),
                run_id: Some(request_id.into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: saved_model.model_hash.clone(),
                    ..Default::default()
                }),
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "hello".into(),
                                message_id: "user-1".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            request_context: Some(pb::RequestContext::default()),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    };
    let append_wire = ai::BidiAppendRequest {
        data: String::new(),
        request_id: Some(ai::BidiRequestId {
            request_id: request_id.into(),
        }),
        append_seqno: 0,
        data_binary: run_client_msg.encode_to_vec(),
    }
    .encode_to_vec();

    let append_resp = app
        .clone()
        .oneshot(
            Request::post("/aiserver.v1.BidiService/BidiAppend")
                .header(header::CONTENT_TYPE, "application/proto")
                .body(Body::from(append_wire))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(append_resp.status(), StatusCode::OK);

    let sse_resp = sse_task.await.unwrap();
    assert_eq!(sse_resp.status(), StatusCode::OK);
    let mut stream = sse_resp.into_body().into_data_stream();
    let mut next_seqno = 1_i64;
    let mut collected_text = String::new();
    let mut saw_end_stream = false;

    while let Some(chunk) = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("RunSSE stream timed out")
    {
        let chunk = chunk.unwrap();
        for (flags, payload) in connect::decode_frames(&chunk).unwrap() {
            if flags & connect::END_STREAM_FLAG != 0 {
                assert_eq!(payload.as_ref(), b"{}");
                saw_end_stream = true;
                break;
            }
            let server_msg = pb::AgentServerMessage::decode(payload).unwrap();
            match server_msg.message {
                Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                    let ack = pb::AgentClientMessage {
                        message: Some(pb::agent_client_message::Message::KvClientMessage(
                            pb::KvClientMessage {
                                id: kv.id,
                                message: Some(pb::kv_client_message::Message::SetBlobResult(
                                    pb::SetBlobResult { error: None },
                                )),
                            },
                        )),
                    };
                    let ack_wire = ai::BidiAppendRequest {
                        data: String::new(),
                        request_id: Some(ai::BidiRequestId {
                            request_id: request_id.into(),
                        }),
                        append_seqno: next_seqno,
                        data_binary: ack.encode_to_vec(),
                    }
                    .encode_to_vec();
                    next_seqno += 1;
                    let ack_resp = app
                        .clone()
                        .oneshot(
                            Request::post("/aiserver.v1.BidiService/BidiAppend")
                                .header(header::CONTENT_TYPE, "application/proto")
                                .body(Body::from(ack_wire))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(ack_resp.status(), StatusCode::OK);
                }
                Some(pb::agent_server_message::Message::InteractionUpdate(update)) => {
                    if let Some(pb::interaction_update::Message::TextDelta(delta)) = update.message
                    {
                        collected_text.push_str(&delta.text);
                    }
                }
                _ => {}
            }
        }
        if saw_end_stream {
            break;
        }
    }

    assert_eq!(collected_text, "pong from byok");
    assert!(saw_end_stream);
}
```

#### 3.1.3 自动化测试与代码质量校验命令

执行者完成上述修改后，按顺序运行：

```bash
# 1. 运行新增的 bidi 单元测试
cargo test --package cursor-server --lib api::cursor::bidi::tests

# 2. 运行 Connect 线级集成测试
cargo test --package cursor-server --test connect_wire

# 3. 安装前端依赖并执行仓库全量门禁（fmt + clippy -D warnings + workspace tests + desktop check）
npm --prefix apps/desktop ci
make check
```

#### 3.1.4 本机 macOS 原生 `arm64` 桌面 App 构建步骤

1. **确认宿主架构与工具链非 Rosetta 状态**：
   ```bash
   uname -m                  # 必须输出 arm64（若输出 x86_64，先用 env /usr/bin/arch -arm64 /bin/zsh 切回原生 shell）
   rustc -vV | grep host     # 本机实测已为 host: aarch64-apple-darwin (rustc 1.96.0)
   ```
2. **前端依赖前置要求（关键）**：
   `Makefile` 第 `35` 行通过 `apps/desktop/node_modules/.bin/tauri signer generate` 生成本地签名密钥，因此首次构建**必须先执行** `npm --prefix apps/desktop ci`，否则 `make build-desktop` 会报找不到 `tauri` 可执行文件。
3. **签名 Key 处理（`LOCAL_TAURI_SIGNING_KEY` 与 macOS 代码签名）**：
   - `apps/desktop/src-tauri/tauri.conf.json` 开启了 `"createUpdaterArtifacts": true`，Tauri 构建时强制要求提供 `TAURI_SIGNING_PRIVATE_KEY`（minisign 私钥，仅用于给构建产物中的 `.app.tar.gz` 生成更新签名，不影响本地运行，也不会替换内嵌的上游 `pubkey`）。
   - `Makefile` 已定义 `LOCAL_TAURI_SIGNING_KEY := $(CURDIR)/.tauri/cursor-byok.local.key`（`/.tauri/` 已在 `.gitignore` 中忽略）。`build-desktop` 目标会自动生成无密码本地临时密钥对（权限 `0600`），并传入 `TAURI_SIGNING_PRIVATE_KEY` 与 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""`。
   - **macOS 原生 `arm64` 代码签名**：Apple Silicon 要求 Mach-O `arm64` 可执行文件必须具备有效签名（哪怕是 ad-hoc 签名 `-`）。参照 `.github/workflows/release.yml:141`，构建时显式注入 `APPLE_SIGNING_IDENTITY="-"`，防止本机钥匙串中过期证书干扰构建。
4. **执行构建命令**：
   - **标准 Makefile 目标（在 `arm64` 原生 shell 下即产出 `arm64` 原生包）**：
     ```bash
     APPLE_SIGNING_IDENTITY="-" make build-desktop
     ```
     - **产物路径**：`target/release/bundle/macos/Cursor BYOK.app`
     - **内部可执行文件**：`target/release/bundle/macos/Cursor BYOK.app/Contents/MacOS/cursor-byok-desktop`
   - **（可选等价方案）显式指定 `--target aarch64-apple-darwin`（彻底免疫 Rosetta 终端环境变量）**：
     ```bash
     make .tauri/cursor-byok.local.key
     TAURI_SIGNING_PRIVATE_KEY="$PWD/.tauri/cursor-byok.local.key" \
     TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
     APPLE_SIGNING_IDENTITY="-" \
     npm --prefix apps/desktop run tauri:build -- --target aarch64-apple-darwin
     ```
     - **显式 target 产物路径**：`target/aarch64-apple-darwin/release/bundle/macos/Cursor BYOK.app`
5. **校验构建产物为原生 `arm64` 且签名完整**：
   ```bash
   file "target/release/bundle/macos/Cursor BYOK.app/Contents/MacOS/cursor-byok-desktop"
   # 期望输出：Mach-O 64-bit executable arm64

   codesign --verify --deep --strict --verbose=2 "target/release/bundle/macos/Cursor BYOK.app"
   ```

#### 3.1.5 端到端验收步骤（不覆盖旧助手、平滑切换与实测验证）

##### 步骤 A：拷贝新 App 到独立验证目录（不动 `/Applications/Cursor助手.app`）

```bash
mkdir -p "$HOME/Applications/cursor-byok-329-stage1"
rm -rf "$HOME/Applications/cursor-byok-329-stage1/Cursor BYOK.app"
cp -R "target/release/bundle/macos/Cursor BYOK.app" "$HOME/Applications/cursor-byok-329-stage1/"
```

##### 步骤 B：配置复用安全性评估与秒级备份（`~/.cursor-byok-v3`）

- **复用安全性**：**安全且 schema 零差异**。本机已装 `1.0.1` 与当前分支均使用完全一致的 SQLite 迁移 `0001..0009`（`pending_count=0`），复用 `~/.cursor-byok-v3` 可直接继承：
  - 已信任的系统根证书 `~/.cursor-byok-v3/ca/`（无需重新弹框输密码装证书）；
  - 全部 14 个模型配置（含 `deepseek-v4.1-flash` → `model_hash = "83f1333d63840a0c"`）；
  - 持久化的端口配置（`service_port = 51866`、`proxy_port = 52148`）与接管开启状态。
- **为什么要先停旧助手再做备份**：`~/.cursor-byok-v3/cursor-byok.db` 当前体积约 **11.5 GB** 且处于 WAL 模式。停掉旧进程让 SQLite 归并 WAL 后，使用 macOS APFS 原生写时复制克隆 **`cp -c`**，可在 **毫秒级、零额外磁盘占用**下完成完整快照备份！

##### 步骤 C：与正在运行的旧助手切换（单实例锁、端口释放与 Cursor 自动重启注意事项）

> **两个关键运行机制提醒（必读）**：
> 1. **Tauri 单实例锁（`tauri-plugin-single-instance`）**：新旧 App 的 `identifier` 同为 `dev.cursorbyok.desktop`。**若旧助手未退出就直接启动新 App，新进程会把唤醒事件发给旧 `x86_64` 进程后立即自行退出！** 此外，直接点窗口红叉只会隐藏到托盘（`api.prevent_exit()`），必须从菜单栏托盘点“退出”或发送 `SIGTERM`。
> 2. **首次代理接管会自动终止一次 Cursor（`local_app/process.rs`）**：旧助手优雅退出时会执行 `harness.disable()` 清理 `Cursor/User/settings.json` 中的 `http.proxy` 并释放 `52148` 端口；新助手启动并打开主界面触发 `harness.status() -> enable()` 时，检测到 `!settings_applied` 会调用 `pkill -x Cursor` 让新代理配置立即生效。**因此切换前请先保存 Cursor 中未保存的编辑器文件！**

按顺序执行切换命令：

```bash
# 1. 备份当前 Cursor settings.json
BACKUP_DIR="$HOME/.cursor-byok-v3-backup-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP_DIR"
cp "$HOME/Library/Application Support/Cursor/User/settings.json" "$BACKUP_DIR/cursor-settings.json"

# 2. 优雅退出正在运行的旧版 Cursor助手（触发 harness.disable 释放 51866/52148 并刷盘 WAL）
kill -TERM $(pgrep -x cursor-byok-desktop)

# 3. 确认旧进程已完全退出，且 51866（管理服务）与 52148（MITM 代理）均无监听残留
while pgrep -x cursor-byok-desktop >/dev/null; do sleep 0.5; done
lsof -nP -iTCP:51866 -iTCP:52148 -sTCP:LISTEN || echo "Ports 51866 & 52148 are clean"

# 4. 使用 APFS 写时复制克隆 11.5GB 数据库及 CA 目录（毫秒级完成，不占双倍空间）
cp -cp "$HOME/.cursor-byok-v3"/cursor-byok.db* "$BACKUP_DIR/"
cp -R "$HOME/.cursor-byok-v3/ca" "$BACKUP_DIR/ca"

# 5. 从独立目录启动新版 arm64 Cursor BYOK.app
open "$HOME/Applications/cursor-byok-329-stage1/Cursor BYOK.app"
```

##### 步骤 D：验证新进程状态、架构与端到端对话

```bash
# 1. 验证运行中的进程路径来自独立目录，且监听已恢复到 51866 和 52148
ps aux | grep cursor-byok-desktop | grep -v grep
lsof -nP -iTCP:51866 -iTCP:52148 -sTCP:LISTEN

# 2. 检查最新日志（注意：日志文件按 UTC 日期命名，例如北京时间 08:00 前为前一日）
LATEST_LOG="$(ls -1t "$HOME/.cursor-byok-v3/logs"/cursor-byok.*.log | head -n 1)"
tail -n 30 "$LATEST_LOG"
# 期望看到：
#   INFO cursor_byok_desktop::desktop: desktop starting version="1.0.1" os="macos" architecture="aarch64" ...
#   INFO cursor_server::app: cursor server listening address=127.0.0.1:51866

# 3. 确认 Cursor settings.json 已重新写入 http://127.0.0.1:52148
grep -E '"http.proxy"|"http.proxySupport"|"disableHttp2"' "$HOME/Library/Application Support/Cursor/User/settings.json"
```

接着进行 Cursor 端到端对话实测：
1. 若步骤 C 中 Cursor 被 `terminate_cursor()` 关闭，重新打开 `/Applications/Cursor.app`（`3.31.3`）。
2. 新建 Agent 对话，模型选择 **`deepseek-v4.1-flash`**，发送测试消息（例如：`请只回复：连接正常`）。
3. **通过标准**：
   - Cursor 正常流式显示回复文本，不再出现 `"Reconnecting..."`；
   - `tail -n 50 "$LATEST_LOG"` 中出现：
     `INFO cursor_server::api::cursor::handlers: routing Cursor Run to BYOK provider request_id="..." model_id="83f1333d63840a0c"`；
   - 日志中**不再出现** `status=400 Bad Request` 或 `BidiAppend data_binary is not part of the captured protocol`。

---

### 阶段 2：后续优化清单（本轮仅列清单与评估，不展开实现）

| # | 优化项 | 核心收益 | 潜在风险 | 工作量估计 |
| :--- | :--- | :--- | :--- | :--- |
| 1 | **arm64 原生包正式替换与构建脚本固化**<br>验收稳定后替换 `/Applications/Cursor助手.app`，并在 `Makefile` 增加可选 `TAURI_BUILD_ARGS` 支持显式 `--target aarch64-apple-darwin`。 | 彻底脱离 Rosetta 转译开销，降低常驻内存与 TLS/SQLite CPU 消耗，启动日志稳定为 `architecture="aarch64"`。 | 替换 `/Applications/Cursor助手.app` 时若自定义了开机自启（LaunchAgent），需确认自启路径与 Bundle 名称（`Cursor助手.app` vs `Cursor BYOK.app`）一致。 | **0.5 人日**（`Makefile` ≈10 行 + 本机替换验证） |
| 2 | **`BootstrapStatsig` 门控加固：显式禁用 `nal_websocket_client`**<br>在 `server/src/cursor/services/analytics.rs` 注入 `nal_websocket_client = false`（及 `run_inference_websocket = false`）。 | 防止 Cursor 官方随时通过 Statsig 全量推送 WebSocket Agent 通道（`wss://api2.cursor.sh/agent/v1/run`），避免首轮握手失败回退带来的延迟或断连。 | 需同步兼容 Statsig 的明文、`djb2`、`sha256` 三种门控键哈希格式（`analytics.rs` 已有现成 `statsig_key()` 工具函数，可直接复用）。 | **0.5 人日**（`analytics.rs` ≈30 行 + 单测） |
| 3 | **升级 `support/cursor-protocol-extractor` 并同步 `protocols/cursor/*.proto`**<br>支持解析 `@bufbuild/protobuf` 的 `static $()` 紧凑描述符、消解多扩展包合并时的重复字段 tag，再更新仓库 `.proto`。 | 恢复 `extract.sh -strict` 对 Cursor 3.29+/3.31 的零错误提取能力，让编译期 `agent_v1.proto` 跟上新增工具与状态字段。 | 更新 `agent_v1.proto` 会移除 `Pi*` 等旧分支并调整少量结构体字段，需全量跑通 `server/` 编译与检查点兼容性测试。 | **1.5～2 人日**（Go 提取器 ≈250 行 + Rust 适配验证） |
| 4 | **广告接口 `tab.leokun.cn` 525 降级与日志降噪**<br>遵循 `ads.rs` 注释保留广告模块，在 `tab.leokun.cn` 返回 5xx/TLS 525 或网络不可达时静默回退本地空广告位/上次缓存，日志改为限频 `debug`/`info`。 | 消除每分钟刷屏的 `WARN cursor_server::error: request failed status=502 Bad Gateway error=provider error: advertisement service failed (525)`，避免掩盖真实故障日志。 | 需确保前端收到空 `slots: []`（`200 OK`）时 UI 无空白占位异常（当前 `AdRuntime` 契约本就支持空数组）。 | **0.5 人日**（`control/service.rs` & `ads.rs` ≈40 行 + 单测） |
| 5 | **失效插件超时隔离与非关键上游 525 观测降噪**<br>排查/清理 `~/.cursor-byok-v3/plugins/installed/antigravity-auth` 定义求值超时；对透传官方 `CppService/AvailableModels`、`FSIsEnabledForUser`、`CppEditHistoryStatus` 的上游 525 做状态说明或可选本地降级。 | 缩短启动阶段插件扫描耗时，减少用户未开启 Tab 服务时的上游 525 干扰项。 | `FSIsEnabledForUser` 等接口仅在未登录官方账号/使用本地伪造 token 时触发上游 525，若做本地 mock 需严格限定在 `request_uses_local_cursor_token` 条件内（同 `compatibility.rs`）。 | **0.5～1 人日** |
| 6 | **新协议可选字段按需增强评估**<br>跟踪 `UserMessageAction.trailing_conversation_history`、`InteractionUpdate.routed_model`、`TurnEndedUpdate.ended_at_ms` 等新字段。 | 若后续 Cursor 版本开始在桌面端实际发送 `trailing_conversation_history`，可提前完成上下文拼接支持。 | 当前 `3.31.3` 客户端 JS 中 `trailingConversationHistory` 调用点为 `0`，过早实现属于推测性代码（违背 `AGENTS.md`），应待捕获到真实非空载荷后再接入。 | **按需（当前 0 行，仅监控）** |

---

## 4. 风险清单与快速回退方案

### 4.1 风险与防范措施

| 风险场景 | 触发原因 | 防范 / 处置办法 |
| :--- | :--- | :--- |
| **启动新 App 后实际运行的仍是旧 `x86_64` 进程** | `tauri-plugin-single-instance` 按 `dev.cursorbyok.desktop` 互斥，旧进程未退出时新进程只会唤起旧窗口并退出。 | 启动新包前必须执行 `pgrep -fl cursor-byok-desktop` 确认为空；启动后核对 `ps aux` 路径及日志中 `architecture="aarch64"`。 |
| **切换助手瞬间 Cursor 窗口被自动关闭** | `CursorHarness::enable()` 在检测到 `settings.json` 未写入当前代理地址时会执行 `pkill -x Cursor`。 | 切换前先保存 Cursor 所有未保存文件；属预期一次性行为，新助手就绪后重开 Cursor 即可。 |
| **端口变化导致已打开的 Cursor 连不上代理** | 若旧助手未退出占着 `52148`，新助手 `bind_proxy_listener` 会自动回退随机端口并改写 `cursor-byok.db` 中的 `proxy_port`。 | 严格遵守“先停旧助手确认 `51866/52148` 释放，再启新助手”，端口即可 100% 保持 `51866` 与 `52148` 不变。 |
| **Cursor 处于 `dev` 更新通道继续后台自动升级** | 本机 `Cursor/User/settings.json` 配有 `"update.releaseTrack": "dev"`（这也是 11-09 升 3.29.16、11-18 升 3.31.3 的原因）。 | 验收通过后建议用户按需评估是否将 `update.releaseTrack` 改回 `"default"`，减少高频协议变动冲击。 |
| **后续官方发布新 GitHub Release 触发自动更新覆盖本地修复包** | `tauri.conf.json` 内嵌上游 `pubkey` 且当前版本号同为 `1.0.1`；若上游发布不含本修复的新版且用户在 UI 点击更新，会覆盖本地包。 | 独立测试目录保留构建产物；若上游新版已合入 `data_binary` 支持则可正常升级，否则重新拷贝本地构建包即可。 |

### 4.2 一键切回旧助手回退步骤（< 30 秒恢复原状）

若阶段 1 新包在验证过程中出现任何异常，按以下 4 步可完整回退至原 `/Applications/Cursor助手.app`：

```bash
# 1. 退出独立目录下的测试版 Cursor BYOK.app，确认进程和端口完全释放
kill -TERM $(pgrep -x cursor-byok-desktop) || true
while pgrep -x cursor-byok-desktop >/dev/null; do sleep 0.5; done
lsof -nP -iTCP:51866 -iTCP:52148 -sTCP:LISTEN || true

# 2. （仅在需要回滚数据库状态时执行；因新旧版本均为迁移 0001..0009，通常无需回滚 DB）
# rm -f "$HOME/.cursor-byok-v3"/cursor-byok.db*
# cp -cp "$BACKUP_DIR"/cursor-byok.db* "$HOME/.cursor-byok-v3/"

# 3. 重新启动原版 /Applications/Cursor助手.app
open "/Applications/Cursor助手.app"

# 4. 打开原版助手主窗口触发接管恢复，校验 51866/52148 监听与 Cursor settings.json 已恢复
lsof -nP -iTCP:51866 -iTCP:52148 -sTCP:LISTEN
grep '"http.proxy"' "$HOME/Library/Application Support/Cursor/User/settings.json"
```
