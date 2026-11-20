# Step 4 启动：证伪两处疑似回归，修正计时，测量首发成本

**结论：没有证据表明这两个现象是 Step 4 引入的性能回归。** 唯一生产代码改动是把 checkpoint 读取的计时起点放回读取前。没有改变模型、代理、存储或 TLS 行为，也没有预热。桌面包未交付，待协调者本机构建及验证。

## 时间线与归因

| 真机观测（协调者校准的 UTC） | 代码与对照 | 结论 |
|---|---|---|
| 02:03:10.780 后端 listen → 02:14:54.760 首批请求；**Cursor 主进程 02:14:48 才启动** | `server/src/app.rs:125` 记录的是 51866 后端；代理在 `server/src/local_app/mod.rs:204` 启动，`server/src/local_app/proxy.rs:48` 才绑定代理监听器。Step 3 在 `app.rs:119` 就是这条后端日志。 | 约 704 秒是两个日志事件之差，其中绝大部分时间 Cursor 没有运行。客户端启动至首个请求约 6 秒，并非代理 TLS 阻塞 11 分钟。该日志不测量 UI 就绪时间。 |
| 首批 forwarded 请求 1330–1952 ms，后续约 300 ms | `server/src/api/cursor/proxy.rs:65` 至 `:113` 包含取得 client、发起请求和等待响应头；`server/src/network.rs:46` 按需创建并缓存 Cursor client。Step 3→4 的两个文件以及 `local_app/proxy.rs` 均无变更。 | 首发慢是真实观测，但日志未拆分本地初始化、DNS、TCP/TLS、网络 RTT 和上游响应。不能据此认定主要时间花在连接池。 |
| 02:16:08 首个 Run；02:16:12 开始工具轮，02:16:15 收到结果，随后记录读取 6864 ms；新 Run 读取 0 ms | 基线 `server/src/timing.rs:26` 自上个 mark 累计；`server/src/run/engine.rs:184` 的同一个循环在每轮读取后 mark。模型和工具往返后再次进入时，标记会含上一轮等待。Step 3 `run/engine.rs:199` 已是同样的计时方式。 | 6864 ms 不是冷启动首次读取，而是首个 Run 内下一轮读取的误归因。`server/src/store/memory.rs:194,541` 锁内存、沿父 checkpoint 遍历并复制消息，并不初始化 tokenizer。修正不扣减 Run 的总耗时。 |

## 隔离测量

临时 HOME、临时 CA（仅测试客户端信任）和环回随机端口。没有启动或停止 Cursor，没有修改真实应用配置或系统信任。下列是实际样本，不是延迟分位数。Rust 日志里的 0 ms 表示小于 1 ms。

| 命令（`cargo test -p cursor-server` 后的参数） | 实测及边界 |
|---|---|
| `--test tool_round provider_tool_use_waits_for_client_result_then_calls_provider_again -- --nocapture` | 模拟客户端延迟 250 ms 回工具结果。**红**：两轮 `load checkpoint messages` 为 `[0, 254] ms`，未达 `<100 ms`，退出 101。**绿**：`[0, 0] ms`；同一工具调用、两次模型请求均完成。 |
| `--test ttft_probe -- --nocapture` | 两个新对话，基线：checkpoint 读取 `0/0 ms`，request context `3/0 ms`，到 provider `20/2 ms`。修后：读取 `0/0 ms`，hydrate 和 base checkpoint import 均 `0/0 ms`，request context `2/0 ms`，到 provider `24/7 ms`。首次包括 mock 客户端上下文往返，第二次命中缓存；测试断言总共只索取一次上下文。测点 `server/src/cursor/compile/run.rs:91,94,150`。 |
| `--lib ready_proxy_serves_first_https_request_to_mock_backend -- --nocapture` | `ProxyRuntime::start` 返回 → 新客户端经 CONNECT/TLS → 本地 mock 后端 → 收全响应 **31.460 ms**，小于 1 s。测试不含 CA 生成、真机进程启动或公网链路；这种路径没有生产代码改动，无修前/修后差异可宣称。 |
| `--lib loopback_forward_reuses_upstream_connection -- --nocapture` | 同一新 `CursorProxy`，三次调用实际 `forward` 到环回 HTTP mock，含读取响应体：**40.018 / 0.470 / 0.465 ms**。mock 回送 peer 地址，三次相同，验证了 TCP 连接复用。该段无 DNS 或上游 TLS；不把它与上一行相加来估算真机耗时。 |

本地冷/热比值虽大，绝对差约 40 ms；测试没有复现真机首批多出的约 1.0–1.65 秒，也无法判定公网握手占比。因此不在启动时主动连上游。若继续诊断真机，需在同一条请求上获得建连、握手和等待响应头的分段时长。

## 变更与验收

以下是修改后文件的物理行数，含空行、注释及内联测试，不含生成代码。

```text
server/
├── src/                          # 生产代码与模块内测试
│   ├── timing.rs                 # 53 行；只重置当前阶段起点，保留 Run 总计时
│   ├── run/engine.rs             # 1096 行；各模型轮次进入 checkpoint 读取前重置
│   ├── api/cursor/proxy.rs       # 249 行；仅加 loopback 转发测量与连接复用断言
│   └── local_app/proxy.rs        # 279 行；仅加 ready → 首个 HTTPS 响应测试
└── tests/tool_round.rs           # 1453 行；在已有工具轮用例断言真实输出的阶段时间
.scratch/byok-lite/step4-startup-fix-report.md  # 本报告
```

`cargo test --workspace --all-targets`：**225 passed, 0 failed**。各目标：

| 目标 | 通过数（依次） |
|---|---|
| desktop lib / main | 3 / 0 |
| server lib / byok-export / cursor-server bin | 140 / 0 / 0 |
| checkpoint_recovery / compaction / connect_wire | 3 / 6 / 10 |
| conversation_delivery / cursor_transport_lifecycle / error_lifecycle | 4 / 2 / 7 |
| interrupt / prefix_stability / tool_round | 17 / 13 / 18 |
| ttft_probe / usage_store | 1 / 1 |

`make check` 退出 0：`cargo fmt --all -- --check`、Clippy `-D warnings`、全量测试、前端两项 `tsc --noEmit` 均通过，Vite 输出 `✓ built in 970ms`。两个代理测试都在 `#[cfg(test)]` 内；生产改动共 6 行。未修改账户/权益路径，未推送。
