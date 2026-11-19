# Step 3：TOML 配置文件 + 14 模型只读导出

## 结论

模型、端口与出站代理现在只来自 `~/.cursor-byok-lite/config.toml`，SQLite 里的
`model_configs` / `network_ports` / `outbound_proxy` 读取与写入路径已全部删除（不再有
「配置 + 数据库」两套来源）。同时新增 `byok-export` 只读导出工具与首次启动迁移
（自动导出模型、逐字节复制 CA）。

- `cargo test --workspace --all-targets`：**全绿**（逐二进制见 §4）
- `make check`：**通过**（fmt + clippy `-D warnings` + cargo test + 前端 typecheck/build）
- 真实库副本导出：**14 / 14 个模型，`model_hash` 全部一致**，源库 mtime 与 sha256 不变
- 产物：arm64、48 MB、`codesign --verify --deep --strict` 通过

---

## 1. 目录与职责

```
server/src/
├── config/                               # 新增：配置是模型与端口的唯一来源
│   ├── mod.rs                    (54)    # 模块出口；应用目录常量、数据库/日志目录解析
│   ├── schema.rs                (554)    # config.toml 解析与校验、env:VAR、provider→model 合并
│   ├── models.rs                (127)    # Models：按配置顺序的模型目录，按 model_hash 查找
│   ├── paths.rs                 (283)    # ~/.cursor-byok-lite 路径 + 首次启动迁移（导出 + CA 复制）
│   ├── export_db.rs             (678)    # mode=ro&immutable=1 只读导出 model_configs / 端口 → TOML
│   ├── settings.rs              (104)    # Settings：listen_addr、database_url、console 装配
│   └── runtime.rs                (29)    # RuntimeConfig：TransportRegistry 需要的配置切片
├── bin/
│   ├── cursor-server.rs                  # 改为 Settings::load()
│   └── byok-export.rs            (68)    # 新增：byok-export --from <db> --out <toml>
├── store/
│   ├── models.rs                         # 删除（DB 模型 CRUD）
│   └── legacy_config.rs                  # 删除（v0.0.49 YAML 导入）
├── control/
│   ├── models.rs                         # 只剩 GET /models 与连通性测试
│   └── settings.rs                       # 端口/代理改为只读 GET
└── network.rs                            # NetworkClients::new(outbound_proxy)，不再读数据库
```

数据流（启动）：

```
~/.cursor-byok-lite/config.toml ──parse──▶ Config{models, default_model, listen_port, proxy_port, outbound_proxy}
        │                                        │
        │ 首次启动（config 不存在且有 ~/.cursor-byok-v3/cursor-byok.db）
        ▼                                        ▼
  export_db::export(ro+immutable)          Settings{listen_addr, database_url, console}
                                                  │
        ┌─────────────────────────────────────────┼──────────────────────────┐
        ▼                     ▼                   ▼                          ▼
  ProviderRouter        TransportRegistry     NetworkClients          CursorHarness
  (models 路由)         (模型目录注入/默认模型)  (outbound_proxy)        (proxy_port, ca_dir)
```

## 2. 配置格式

`~/.cursor-byok-lite/config.toml`，字段与 plan.md 3.3 节一致：

| 段 | 字段 |
|---|---|
| 顶层 | `default_model`（`display_name` 或 16 位 `model_hash`） |
| `[server]` | `listen_port`(默认 51866)、`proxy_port`(默认 52148)、`request_timeout_secs`(3600)、`stream_idle_timeout_secs`(1800)、可选 `outbound_proxy` |
| `[stats]` | `retention_days`、`max_rows`（第 4 步使用，这一步只解析校验） |
| `[[providers]]` | `name`、`type = "openai"\|"anthropic"`、`base_url`、`api_key`（支持 `env:VAR` 或明文）、可选 `custom_headers` |
| `[[models]]` | `provider`、`display_name`、`model_id`、`endpoint`、可选 `model_hash`、`reasoning_effort`、`thinking_effort`、`context_window_tokens`、`max_completion_tokens`、`anthropic_max_tokens`、`thinking_budget_tokens`、`use_full_url`、`group_name`、`tooltip_data`、`base_url`、`openai_extra_params`、`anthropic_extra_params` |

`model_hash` 语义：填写即原样使用；省略时按 `server/src/model/configuration.rs`
既有算法（`Sha256(request_url \n model_id \n api_key \n display_name [\n endpoint])[..8]`）
计算，导出时一律写明原 hash。

## 3. 删除与改造

### 删除

| 路径 | 说明 |
|---|---|
| `server/src/store/models.rs`（401 行） | `model_configs` 的 CRUD：`create_models` / `update_model` / `delete_model` / `reorder_models` / `models()` |
| `server/src/store/legacy_config.rs`（276 行） | v0.0.49 YAML 模型导入 |
| `store/settings.rs` 的模型/端口/代理读写 | `port_settings`、`set_port_settings`、`set_service_port`、`set_proxy_port`、`proxy_settings_secret`、`set_proxy_settings`、`ProxySettingsSecret`、`ProxySettingsInput` |
| `control` 写接口 | `POST /models`、`PUT /models/{hash}`、`DELETE /models/{hash}`、`PUT /models/order`、`POST /models/discover`、`GET|POST /models/import-v0049`、`PUT /settings/ports`、`PUT /settings/proxy` |
| `control/service.rs` 对应方法 | `create_models`、`update_model`、`delete_model`、`reorder_models`、`discover_models`、`import_v0049_models`、`preview_v0049_models` |
| `apps/desktop/src/features/models/` | `CursorModelEditor.tsx`、`LegacyModelImport.tsx`(+scss)、`CursorPresetChips.tsx`(+scss) |
| 前端写路径 | `api.createModels` / `updateModel` / `deleteModel` / `reorderModels` / `discoverModels` / `importV0049Models` / `previewV0049Models` / `setPorts` / `setProxySettings` 及 `appStore` 对应动作 |

### 改造为配置驱动

| 文件 | 改动 |
|---|---|
| `provider/router.rs` | `ProviderRouter::new(store, Models, clients, timeouts)`，按 `model_hash` 从配置查模型 |
| `cursor/services/model_catalog.rs` | 模型目录、`GetDefaultModel` / `GetDefaultModelForCli` / nudge 全部读 `registry.models()` 与 `config.default_model` |
| `api/cursor/handlers.rs` | BidiAppend 的本地路由判定改为 `registry.models().contains(model_id)` |
| `cursor/compile/run.rs` | `models.get(&model.model_id)` 取代 `store.model()` |
| `cursor/transport/registry.rs` | 新增 `models()` / `default_model()`，注入 `RuntimeConfig` |
| `cursor/conversation/{registry,runtime}.rs` | 依赖改为 `RuntimeConfig`，工具层拿 `outbound_proxy` |
| `cursor/tools/mod.rs` | `WebSearch/WebFetch` 从 `Store` 改为 `Option<String>` 代理参数 |
| `search/{fetch,federation}.rs` | 同上，`client_builder(Option<&str>)` |
| `app.rs` | 删除 `use_persisted_ports` 与端口回写，`App::new(Settings)` |
| `local_app/mod.rs` | `CursorHarness::new(store, proxy_port, model_count, ca_dir)`，不再写回代理端口 |
| `local_app/ca/mod.rs` | `CaManager::managed(ca_dir)`，CA 目录由配置路径决定 |
| `config.rs` → `config/` | 原 `Config::from_env/desktop`、`CURSOR_LISTEN_ADDR`/`CURSOR_DATABASE_URL` 全部移除 |

### 前端（只读）

- 模型页：只读网格 + 分组列表，保留连通性测试；顶部提示「编辑 ~/.cursor-byok-lite/config.toml 后重启生效」
- 设置页：端口、出站代理由可编辑改为只读展示，指向配置文件
- 空模型时提示「在 ~/.cursor-byok-lite/config.toml 中添加 `[[models]]` 后重启。」
- 只改到能编译能显示：`npx tsc --noEmit` 与 `vite build` 通过，UI 未做其他调整（第 5 步整体替换）

### 保持不变（按任务要求）

- SQLite 路径仍是 `~/.cursor-byok-v3/cursor-byok.db`，日志目录不变
- `~/.cursor-byok-v3/` 只读访问；未创建也未写入 `~/.cursor-byok-lite/`
- 配置重载只做「重启生效」，不做文件监控

## 4. 测试

### 4.1`cargo test --workspace --all-targets`

```
cursor_byok_desktop (lib)                 3 passed
cursor_byok_desktop (main)                0 passed
cursor_server (lib)                     145 passed
byok-export (bin)                         0 passed
cursor-server (bin)                       0 passed
checkpoint_recovery                       4 passed
compaction                                6 passed
connect_wire                             10 passed
conversation_delivery                     4 passed
cursor_transport_lifecycle                2 passed
error_lifecycle                           7 passed
interrupt                                17 passed
prefix_stability                         13 passed
tool_round                               18 passed
ttft_probe                                1 passed
------------------------------------------------------------------------
合计                                    230 passed, 0 failed
```

`make check`（fmt --check + clippy -D warnings + cargo test + `npm run check`）：exit 0。

### 4.2 新增测试

| 测试 | 覆盖 |
|---|---|
| `config::schema::parses_the_documented_configuration` | plan.md 完整示例（含 `[server]`/`[stats]`/`custom_headers`/端到端 URL） |
| `config::schema::generated_hashes_match_the_legacy_algorithm` | 省略 `model_hash` 时与原算法一致 |
| `config::schema::rejects_unset_env_references` | `env:VAR` 未设置时报错 |
| `config::schema::rejects_unknown_reasoning_effort` / `..._missing_required_fields` / `..._unknown_provider_references` / `..._an_unknown_default_model` | 缺字段与非法值报错 |
| `config::schema::defaults_apply_when_sections_are_omitted` | `[server]` 省略时的默认端口/超时 |
| `model::configuration::configured_models_reproduce_the_legacy_hash` | 4 组参数（chat / responses / use_full_url / anthropic）新旧算法逐一相等，且声明 hash 优先 |
| `config::export_db::export_round_trips_models_and_keeps_the_source_untouched` | 临时库 → 导出 → 用生产 loader 解析，hash 一致；源库 mtime + sha256 不变；输出权限 0600 |
| `config::export_db::export_refuses_to_overwrite_an_existing_file` | `--out` 已存在时拒绝覆盖 |
| `config::export_db::export_keeps_declared_hashes_even_when_parameters_change` | 把 api_key 改成 `env:VAR` 后 hash 仍不变 |
| `config::paths::first_start_exports_models_and_copies_the_ca_byte_for_byte` | 临时 HOME：自动导出 config.toml；CA 逐字节一致，目录 0700 / 文件 0600 |
| `config::paths::an_existing_configuration_is_never_overwritten` | 已有 config.toml 时不再导出、不再复制 CA |
| `config::models::resolves_default_by_hash_or_display_name` / `rejects_duplicate_hashes` | 默认模型解析与重复 hash |

集成测试原先依赖 `store.create_model()` 播种模型，现改为 `fixtures::model(...)` +
`fixtures::runtime(...)` 直接构造配置模型（DB 不再承载模型）。

### 4.3 真实库副本只读验证

```bash
$ cp -c ~/.cursor-byok-v3/cursor-byok.db /tmp/byok-step3-final.vTSROk/
$ cargo run -p cursor-server --bin byok-export -- \
    --from /tmp/byok-step3-final.vTSROk/cursor-byok.db \
    --out  /tmp/byok-step3-final.vTSROk/config.toml
exported 14 models from 9 providers into /tmp/byok-step3-final.vTSROk/config.toml
```

```
db model_configs rows : 14
exported [[models]]   : 14
hash set identical    : True
(name, hash) identical: True
  dffde0e2d7478366  claude-opus-5-5
  bb2ef95154fa0bde  claude-sonnet-5-5cc
  2823e3f163cc7e85  cline-free/muse-spark-1.3-contributor
  83f1333d63840a0c  deepseek-v4.1-flash
  59fe6e1d0ff6785a  glm-5.3 副本
  fc05255f1b5f4703  gpt-5.6-luna
  e70810281b6543ff  gpt-5.6-sol
  c3d9e5cc0d2d3e22  gpt-6-astra
  e380e4274d7630d4  gpt-6-astra
  d0a2bd5923c6de69  gpt-image-2.5-flare
  79d3ade70efb43f9  grok-4.6
  56c735568f765ed5  grok-4.7
  4620d8239e0f7f8b  kimi-k3-1
  921aa2692610803c  opus-5.5
RESULT: ALL 14 HASHES MATCH
```

源库未被改动：

```
before_mtime=1795095912
d3da0ea0754c7296681ece46328bc236086cf1ad5c7481005c5369707a58b0d9  cursor-byok.db
after_mtime=1795095912
SOURCE_DB_UNCHANGED
```

导出的 `[server]` 与（打码的）供应商：

```
default_model = "gpt-image-2.5-flare"
listen_port = 51866
proxy_port = 52148
request_timeout_secs = 3600
stream_idle_timeout_secs = 1800

  api.ziyouxie.online          anthropic https://api.ziyouxie.online     api_key=sk-c***
  127.0.0.1-8899               openai    http://127.0.0.1:8899/v1        api_key=n2a-***
  192.168.11.46-7863           openai    http://192.168.11.46:7863/v1    api_key=8f41***
  192.168.11.46-7863-2         openai    http://192.168.11.46:7863/v1    api_key=a39b***
  192.168.11.46-8000           openai    http://192.168.11.46:8000/v1    api_key=g2a_***
  localhost-7863               openai    http://localhost:7863/v1        api_key=9bb2***
  api.ziyouxie.online-2        openai    https://api.ziyouxie.online     api_key=sk-0***
  api.ziyouxie.online-3        openai    https://api.ziyouxie.online     api_key=sk-c***
  api.ziyouxie.online-4        openai    https://api.ziyouxie.online     api_key=sk-f***
```

（同 host + 同 key 归并为一个 `[[providers]]`；14 个模型归并为 9 个供应商。导出文件权限为
`-rw-------`。副本与临时 toml 已删除，`~/.cursor-byok-lite/` 未被创建。）

## 5. 构建产物

```bash
$ APPLE_SIGNING_IDENTITY="-" make build-desktop
    Finished `release` profile [optimized] target(s) in 2m 14s

$ du -sh ~/Applications/cursor-byok-lite-step3/Cursor\ BYOK.app
 48M

$ file ~/Applications/cursor-byok-lite-step3/Cursor\ BYOK.app/Contents/MacOS/cursor-byok-desktop
Mach-O 64-bit executable arm64

$ codesign --verify --deep --strict --verbose=2 ~/Applications/cursor-byok-lite-step3/Cursor\ BYOK.app
Cursor BYOK.app: valid on disk
Cursor BYOK.app: satisfies its Designated Requirement
```

`~/Applications/cursor-byok-lite-step3` 原本不存在，由本步骤创建；未覆盖任何已有目录。

## 6. 变更量

```
$ git diff --stat 979db95..HEAD | tail -1
 68 files changed, 3042 insertions(+), 4786 deletions(-)
```

## 7. 未做（留给后续步骤）

- 第 4 步：内存 LRU + `usage.db` / CSV、`context_sync` 提速、删除剩余 SQLite 表
- 第 5 步：拆除 Tauri 与 Web 前端，换原生菜单栏（本步骤的只读模型页届时整体替换）
- 配置热重载（本步骤明确只做「重启生效」）
