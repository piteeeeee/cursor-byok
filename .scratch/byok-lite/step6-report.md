# Step 6：依赖树大扫除 + 量化指标验收（byok-lite 收官）

以 `5d96043` 为基线（已含 Step 1~5）。分支 `lite/step6-deps`。
本步**只动依赖与量化验收**：删 2 个零引用依赖、收窄 2 处 feature，不改任何业务代码。
`.app` 已构建并实测 17 MB（< 20 MB 达标）；RSS 与冷启动需要启动菜单栏进程，**未启动，留给协调者**。

## 结论先行

| 指标 | 目标（plan 3.6） | 实测 | 状态 |
| :--- | :--- | :--- | :--- |
| `.app` 体积 | `< 20 MB` | **17 MB**（17,300 KB，`du -sk`） | ✅ 达标 |
| 架构 | arm64 | `Mach-O 64-bit executable arm64` | ✅ 达标 |
| 首 token 本地延迟 | `< 15 ms` | **7~8 ms**（第 2 轮，命中上下文缓存）；第 1 轮 2~22 ms | ✅ 达标 |
| `usage.db`（20 次调用） | `< 30 MB` | **20,480 B（20 KB）** | ✅ 达标（远超） |
| 常驻 RSS | `< 25 MB` | 未测 | ⏳ **待协调者**（需启动应用，本任务禁止） |
| 冷启动 | `< 0.4 s` | 未测 | ⏳ **待协调者**（需启动应用，本任务禁止） |

三道门全绿：`cargo fmt --all -- --check` 零输出、`cargo clippy --workspace --all-targets -- -D warnings` 零警告、
`cargo test --workspace --all-targets` 15 个测试二进制全绿。

## 删了什么、凭什么确认

判据统一为：`cargo tree -i <crate>` 看反向依赖 + 按 crate 的**真实 use 名**（连字符换下划线）在
`server/src`、`server/tests`、`server/build.rs`、`apps/menubar/src` 里 grep 引用文件数。两者都为「只有
cursor-server 自己列在 manifest 里、源码 0 引用」才删。

| crate | 出处 | 反向依赖（删前） | 源码引用 | 处理 |
| :--- | :--- | :--- | :--- | :--- |
| `serde_yaml` | `server/Cargo.toml` | `cargo tree -i serde_yaml` → 仅 `cursor-server` 自己 | `serde_yaml` 0 文件 | **删除** |
| `objc2` | `apps/menubar/Cargo.toml` `[target.'cfg(target_os="macos")']` | `cargo tree -i objc2` → 只经由 muda/tao/tray-icon 传递 | `objc2`/`msg_send!`/`sel!` 0 文件；menubar 只用 `libc` 的 `getrlimit`/`setrlimit`/`flock` | **删除** |
| `reqwest` feature `blocking` | `server/Cargo.toml` | `reqwest::blocking` 0 文件（源码里的 `blocking` 全是 `tokio::task::spawn_blocking`） | — | **收窄 feature** |
| `sqlx` 默认 feature（`any`+`macros`+`migrate`+`json`） | `server/Cargo.toml` | `sqlx::query!`/`query_as!`/`query_scalar!` 宏 0 处、`sqlx::Any` 0 处、`sqlx::types`/`sqlx::json` 0 处 | `sqlx::migrate::MigrateError` 仍用（`server/src/error.rs:29`） | **改为 `default-features = false`，只留 `migrate` + `runtime-tokio` + `sqlite`** |

`Cargo.lock` 因此少 2 个包、并摘掉 sqlx 的 mysql/postgres 后端：
`serde_yaml 0.9.34+deprecated`、`unsafe-libyaml 0.2.11` 整块消失，`sqlx` 不再依赖 `sqlx-mysql`/`sqlx-postgres`。
包总数 `589 → 587`。

### 核实后**不删**的（任务点名但实际仍在用）

这些是任务文件与 plan 里提到「应已随 semble 移除」的候选，逐一验证后确认**要么还在主链路、要么根本不在依赖树里**：

| crate | 结论 | 证据 |
| :--- | :--- | :--- |
| `include_dir` | **保留**（任务文件说"确认无引用后删"，但它有引用） | `server/src/cursor/prompting/assets.rs:8` 用 `include_dir!("$CARGO_MANIFEST_DIR/prompt/cursor")` 把 7 个模式的 prompt 内嵌进二进制 |
| `json5` | **保留**（plan 明确「仅 settings.rs 需要则保留」） | `server/src/local_app/settings.rs:46` `json5::from_str` 解析 Cursor 的 JSONC `settings.json` |
| `zip` / `rayon` / `bincode` / `half` / `tree-sitter`×8 / `tokenizers` / `safetensors` | **本就不在依赖树里**（Step 1 随 `semble-core` 已删） | `Cargo.lock` 中 `^name = "<crate>"` 计数均为 0；`cargo tree -i` 报 "did not match any packages" |
| `time` / `chrono` | **两者都保留**（不是二选一） | `time` 只在 `server/src/local_app/ca/mod.rs:19` 的 `OffsetDateTime`/`Duration`（rcgen 的 CA 有效期）；`chrono` 在 5 个文件里做时间戳与 `chrono_tz` |

`sqlx` 按 AGENTS.md「优先复用现有依赖」**保留单表实现**，没有重写存储：`usage_records` 一张表 + 一个
`idx_usage_ts` 索引，`UsageStore` 的 open/record/summary/export_csv/prune 全不动。

## 保命代码与账户/权益代码：一行未改

`git diff 5d96043..HEAD` 只触及 `Cargo.lock`、`server/Cargo.toml`、`apps/menubar/Cargo.toml` 三个文件，
以及本报告。逐项核对 plan 5.1 的保留清单：

```text
server/src/cursor/services/analytics.rs          # 门控钉死 nal_websocket_client=false / agent_host_local_loop=false，未动
server/src/local_app/mod.rs                      # RunInference permission_denied 回退、harness.cleanup_stale_settings 未动
server/src/local_app/ca/mod.rs                   # 从 ~/.cursor-byok-v3/ca 只读复用 + CA 复用校验未动
server/src/cursor/services/blob_sync.rs          # 内存未命中走 GetBlobArgs 回源未动
server/src/config/store.rs                       # config 校验失败保留上次有效配置未动
server/src/cursor/services/account.rs            # 账户/权益代码未动
server/src/cursor/services/entitlement.rs        # 权益代码未动
```

（`data_binary` XOR 解码同样未动。）删 `serde_yaml` 与收窄 feature 都不触碰这些路径——这一点由
「除三份清单外零 diff」直接保证。

## 量化指标逐项

### 1. `.app` 体积与架构

```text
$ make build-app
   Compiling cursor-byok-lite v0.1.0 (…/apps/menubar)
    Finished `release` profile [optimized] target(s) in 4m 13s
built target/release/bundle/Cursor BYOK Lite.app

$ du -sh "target/release/bundle/Cursor BYOK Lite.app"
 17M	target/release/bundle/Cursor BYOK Lite.app
$ du -sk "target/release/bundle/Cursor BYOK Lite.app"
17300	target/release/bundle/Cursor BYOK Lite.app

$ file "target/release/bundle/Cursor BYOK Lite.app/Contents/MacOS/cursor-byok-lite"
…: Mach-O 64-bit executable arm64

$ du -sh .../Contents/MacOS/cursor-byok-lite .../Contents/Resources .../Contents/Info.plist
 17M	…/Contents/MacOS/cursor-byok-lite      # 单二进制，无 WebKit、无 Resources
  0B	…/Contents/Resources                   # 空
4.0K	…/Contents/Info.plist
```

`.app` 全部体积来自那一个 stripped 二进制（`lto`+`codegen-units=1`+`panic=abort`+`strip`）。
对比 plan 3.6 的基线 64 MB（Tauri）与估算「方案 B < 18 MB」，实测 17 MB 与估算吻合。

### 2. 首 token 本地延迟

`server/tests/ttft_probe.rs` 用生产代码跑完整 Run 前置链路（`TransportRegistry` → `ConversationRuntime`
→ `compile::prepare` → `RunEngine::run_claimed` → 首次 `provider.stream`），测试进程扮演 Cursor 回
`SetBlobResult`/`GetBlobArgs`/`RequestContextResult`，所以只含助手内部耗时：

```text
$ cargo test -p cursor-server --test ttft_probe -- --nocapture
iteration 0: append -> first provider request 16 ms   # 冷路径，含首次上下文解析
iteration 1: append -> first provider request 8 ms    # 命中 RequestContext 内存缓存
test result: ok. 1 passed; 0 failed
```

第 2 轮即 plan 3.6 定义的口径（「命中 RequestContext 内存缓存时」），**7~8 ms < 15 ms**。同机多次采样
第 2 轮落在 6~12 ms；第 1 轮（冷路径，含首次上下文解析）落在 2~22 ms，不计入该口径。

> **关于该测试的 15 ms 硬断言**：它在**基线 `5d96043` 上同样会偶发红**。我用 step5 工作树里同 commit
> 的预编译二进制（`target/debug/deps/ttft_probe-11b4fb3dfe59461c`，未重新编译）连跑 8 次，8 次里 2 次
> 以 `iteration 1 took 16/17 ms` 失败——**与本步的依赖改动无关**，是本机负载抖动（`uptime` 采样
> load average 8.8~26.6，51 个用户会话）。本步**没有**改这个断言或测试代码以免扩大范围。全量
> `cargo test --workspace --all-targets` 在空闲窗口连续三次 exit=0、15 个二进制全绿。

### 3. `usage.db` 体积（20 次调用）

`server/tests/usage_store.rs::twenty_local_provider_calls_are_recorded_without_payloads` 用真实
`App`+`UsageStore` 在**临时 HOME** 下走 20 次 provider 调用并落库：

```text
$ cargo test -p cursor-server --test usage_store -- --nocapture twenty_local_provider_calls
usage.db: 20480 bytes; records: 20
test result: ok. 1 passed; 0 failed
```

**20 KB 对 30 MB 上限**——两个数量级余量。表中只有一次调用的模型标识、token 计数、耗时、状态，
`usage_records` 不存请求/响应/会话内容（测试同时断言 db 里不含响应正文）。

### 4. 待协调者：常驻 RSS 与冷启动

两项都需要**启动 `Cursor BYOK Lite.app`** 才能测（RSS 要读进程、冷启动要从拉起进程计到 `51866`
开始监听）。任务明确禁止我启动应用，故**未测、未启动任何进程**。留给协调者的命令：

```bash
open "target/release/bundle/Cursor BYOK Lite.app"
ps -o pid,rss,comm -p $(pgrep -x cursor-byok-lite)      # 目标 RSS < 25 MB
lsof -nP -iTCP:51866 -sTCP:LISTEN                        # 冷启动 < 0.4 s（配合计时起点）
pgrep -l WebContent                                      # 应为空：无 WebKit 子进程
```

已构建的 `.app` 留在 `target/release/bundle/`，未写 `~/Applications/`、未 push。

## 目录树（本步触及的部分）

```text
cursor-byok/
├── Cargo.lock                    -2 包：serde_yaml / unsafe-libyaml；sqlx 摘掉 mysql+postgres 后端
├── server/
│   └── Cargo.toml                -1 依赖 serde_yaml；reqwest 去掉 blocking；sqlx 收成 migrate+sqlite
├── apps/menubar/
│   └── Cargo.toml                -1 依赖 objc2（连同它那条 [target.'cfg(target_os="macos")'] 段）
└── .scratch/byok-lite/
    └── step6-report.md           本报告
```

`server/src/`、`server/tests/`、`apps/menubar/src/` 全部 0 行改动。

## 验收命令与输出

```text
$ cargo fmt --all -- --check
（零输出，exit 0）

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.81s
（零警告，exit 0）

$ cargo test --workspace --all-targets
cursor_server (lib)                146 passed
cursor_byok_lite (bin)               0 passed
byok_export (bin)                    0 passed
cursor_server (bin)                  0 passed
checkpoint_recovery                  3 passed
compaction                           6 passed
connect_wire                        10 passed
conversation_delivery                4 passed
cursor_transport_lifecycle           2 passed
error_lifecycle                      7 passed
interrupt                           17 passed
prefix_stability                    13 passed
tool_round                          18 passed
ttft_probe                           1 passed
usage_store                          2 passed
                                    共 229 passed, 0 failed（连续 3 次 exit=0）

$ git diff --stat 5d96043..HEAD | tail -1
 3 files changed, 2 insertions(+), 32 deletions(-)     # 本报告的 diff 在提交后并入
```

## 已知边界 / 留给协调者

1. **RSS 与冷启动未测**（需启动应用，本任务禁止）。命令见上，`.app` 已在 `target/release/bundle/`。
2. **真机 Cursor 端到端未验证**：本步不改运行时代码，理论上无行为变化；但「删依赖后仍能接管并对话」
   只能在协调者起来后确认。
3. **`ttft_probe` 的 15 ms 硬断言在基线上就会偶发失败**（已在同 commit 预编译二进制上复现 2/8）。
   若希望收掉这个抖动，建议后续把它改成「取 5 次中位数」或放宽到 25 ms——**本步未改**，避免超出范围。
