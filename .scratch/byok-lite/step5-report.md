# Step 5：拆 Tauri+React，换纯 Rust 菜单栏 + server 库化

以 `7daa1e0` 为基线（已含 Step 1~4）。`apps/desktop/` 与 `server/src/control/` 整目录删除，新增
`apps/menubar/` 单二进制菜单栏，`cursor-server` 收成四类型公开面的 headless 库。
`.app` bundle 与真机 Cursor 验证按分工留给协调者。

## 结构变化

```text
cursor-byok/
├── apps/
│   ├── desktop/                                  # 删除 17,714 行
│   │   ├── src/                                  # React 前端：features/shell/shared/i18n/styles
│   │   └── src-tauri/                            # Tauri 桌面生命周期、插件、updater
│   └── menubar/                                  # 新增，纯 Rust，单二进制
│       ├── Cargo.toml                (24)        # tray-icon 0.24 / muda 0.19 / tao 0.35 / png
│       ├── Info.plist                (31)        # LSUIElement=true，无 Dock 图标
│       ├── assets/trayTemplate.png   (158 B)     # 32×32 黑白模板图，AppKit 当掩码渲染
│       └── src/
│           ├── main.rs               (198)       # flock 单实例、日志、main 线程 AppKit + Tokio 运行时
│           ├── tray.rs               (201)       # 13 行菜单构建与重绘、点击 id → Action 映射
│           ├── actions.rs            (287)       # 每个菜单动作 = 一次 cursor_server 调用 + open(1)
│           └── resource_limits.rs    (36)        # RLIMIT_NOFILE 抬到 65,536
├── server/
│   ├── src/
│   │   ├── lib.rs                    (27)        # 对外只有 App / ConfigStore / UsageStore / CursorHarness
│   │   ├── app.rs                    (170)       # 组装对象图；bind/serve/serve_on/run
│   │   ├── config/
│   │   │   ├── store.rs              (445)       # 新增：ConfigStore，运行期热重载与 default_model 回写
│   │   │   ├── settings.rs                       # 删除：Settings/Settings::load/ConsoleSource
│   │   │   └── runtime.rs            (11)        # RuntimeConfig 收成 models + default_model + outbound_proxy
│   │   ├── provider/
│   │   │   ├── config.rs             (21)        # 新增：ProviderConfig/ProviderKind 从 config 移入 provider
│   │   │   └── router.rs                         # 每次调用读 ConfigStore，不再持有 models 快照
│   │   ├── local_app/mod.rs          (321)       # CursorHarness::new(&ConfigStore)，status/report/enable/disable
│   │   └── search/cache.rs                       # WebCache 目录由调用方给，不再自己读 HOME
│   └── src/control/                              # 删除 674 行：Web Console 路由 + API 适配器
├── .agents/skills/                               # 删除 frontend / floating-ui / i18n / model-selection / release
├── .github/workflows/{ci.yml,release.yml}         # CI 改为纯 Rust；Tauri 发布流程删除
├── Dockerfile                                    # 去掉 web 构建阶段与 console 目录
└── Makefile                                      # check / dev-server / build-server / build-app / dmg
```

数字为 `wc -l` 物理行，含空行与内联测试。

## server 库 API 形状

`lib.rs` 只 `pub use` 四个类型，其余模块仍可通过 `cursor_server::config::…` 等路径取用（测试与
`byok-export` 用得到），但嵌入方只需要这四个。

```text
App                     # 一个对象图，无 GUI 依赖
├── new(ConfigStore) -> Result<App>          # 建 MemoryStore + UsageStore + ProviderRouter
│                                            #   + TransportRegistry + CursorHarness + router
├── config() -> &ConfigStore
├── usage()  -> &UsageStore
├── harness() -> &CursorHarness
├── bind() -> Result<TcpListener>            # 先占端口，调用方随即拿到地址
├── serve_on(listener, CancellationToken)    # 用已绑定的 listener 服务
├── serve(CancellationToken)                 # bind + serve_on
└── run()                                    # 服务到 SIGINT/SIGTERM

ConfigStore              # ~/.cursor-byok-lite/config.toml 的运行期形象
├── load() / at(Paths)                       # 读文件，首次启动迁移
├── in_memory(Paths, Models, default)        # 无文件：供嵌入方与测试注入配置
├── models() -> Arc<Models> / catalog()
├── default_model() -> String
├── set_default_model(&str) -> Result<String># 校验 → 只重写 default_model 那一行 → 改内存
├── reload() -> Result<()>                   # 端口/超时/代理/统计上限变更则拒绝并报字段名
└── paths() / listen_addr() / proxy_port() / request_timeout() / stream_idle_timeout()

UsageStore               # usage.db 单表
├── today_and_month_summary() -> UsageSummary
├── latest_error() -> Option<String>         # 新增：菜单「最近错误」用
└── export_csv(path)

CursorHarness            # 接管开关
├── new(&ConfigStore)
├── status() / report()                      # status 会按保存的选择自我修复；report 只读
├── set_enabled(bool) / enable() / disable()
├── initialize_ca()
├── set_backend_addr(SocketAddr) / service_port()
└── cleanup_stale_settings()
```

为 workbuddy-switch 进程内嵌入做的三点：**不持有 HOME 路径推导**（`Paths` 由调用方给）、
**不自己建运行时**（`App` 全是 async 方法，运行时归宿主）、**配置可变而无需重启**
（模型目录与默认模型每次请求现读，`reload()` 只放行能就地生效的字段）。
`App` 是 `Clone`：`serve_on` 取走一个副本，宿主手里那份继续能读状态、切模型。

## 菜单

`状态: 接管中 (51866 / 52148)` / `已暂停` / `待初始化 CA` · `默认模型:` · `今日 Token | 本月` ·
`最近错误:`（仅有时出现）· 分隔线 · `暂停接管`/`开启接管` · `切换默认模型 ▸`（列出 config.toml
全部模型、勾选当前）· `重载配置` · `导出用量 CSV…`（写 `exports/usage-<ts>.csv` 并 Finder 显示）·
`打开配置文件` · `打开日志目录` · 分隔线 · `退出`（先 `disable()` 还原 settings.json 再停服）。

只读行每 30 秒重算一次；每次重绘都走 `report()`，绝不改写 Cursor 配置。

## 验收证据

```text
$ cargo test --workspace --all-targets
cursor_server (lib)                146 passed
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
                                    共 229 passed, 0 failed

$ make check                 # fmt + clippy -D warnings + 全量测试
$ cargo build --release
target/release/cursor-byok-lite: Mach-O 64-bit executable arm64   17 MB
```

## 本步修掉的两个既有测试缺陷

1. **`provider_tool_use_waits_for_client_result_then_calls_provider_again` 在基线上就红**（5/5 失败）。
   它用 `tracing::subscriber::set_default` 抓 `cursor_server::timing` 的阶段计时，但 transport actor
   与 Run 跑在运行时 worker 线程上，线程局部 subscriber 抓不到。改成断言该用例真正关心的事
   （250 ms 工具等待后仍是 2 次 provider 调用、Run 仍是同一个），阶段计时改由
   `server/src/timing.rs` 的单元测试直接覆盖。
2. **`attempt` / `proxy` 两个用例在本机一直红**：机器的 macOS 系统代理（127.0.0.1:7897）被
   hyper-util 的 matcher 套到了 loopback 上。`NetworkClients::direct()` 给回环流量用不代理的
   client，两个用例因此与本机代理设置无关。同一条路径也让 `prefetched blobs` 的 loopback
   转发不再绕出系统代理。

## 已知边界

- **本机无法真机验证 Cursor**：没启动 Cursor，`settings.json` 写入/还原只在临时 HOME 里验证过。
  实测：`initialize_ca()` 正确落到 `Untrusted`（生成 CA 但不信任），`set_enabled(true)` 因此拒绝
  ——这是设计上的安全默认，协调者装好信任后才谈「接管中」。
- 真机 `~/Library/Application Support/Cursor/User/settings.json` 与 `~/.cursor-byok-lite/` 全程未改
  （前者 SHA-1 前后一致：`3abc9e06…`；后者文件时间戳均早于本次会话）。
- **`.app` 未构建，待协调者**：`make build-app` 目标已就绪（裁剪二进制 + Info.plist + PkgInfo +
  ad-hoc codesign），按分工由协调者执行 bundle/签名/装包与真机验证。
- 菜单「导出用量 CSV」与「重载配置」在本机只做过库层验证（`export_csv`/`reload` 的单元测试），
  未在真机菜单里点过。
