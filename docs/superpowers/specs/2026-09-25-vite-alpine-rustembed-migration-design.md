# 前端工程化迁移：Vite 6 + Alpine.js 3 + rust-embed 单文件二进制

**日期**：2026-09-25
**状态**：设计已确认，待转实施计划
**目标读者**：执行本次迁移的开发者

---

## 1. 背景与目标

当前前端是 `src/index.html` 单文件（约 2500 行，内联命令式 JS），样式由 `build.rs` 调用 Tailwind CLI 生成到 `templates/style.css`，再由 `main.rs:2919` 用 `include_str!("../templates/style.css")` 编译期嵌入；HTML 由 `main.rs:1770` / `main.rs:1777` 的 `include_str!("index.html")` / `include_str!("login.html")` 嵌入。

存在三个问题：

1. **命令式 DOM 冗长**：`document.getElementById` + `innerHTML` 拼接 + 手工 `classList` 切换，状态与视图同步靠人工维护，易漏。
2. **构建链割裂**：Tailwind CLI 由 `build.rs` 直接 `npx` 调用，`package.json` 不入库，构建配置不可复现；`src/login.html` 未登记 `rerun-if-changed`，改登录页不触发重建。
3. **打包靠内联**：所有 JS/CSS 内联在单个 HTML 中，无法分包、无法按模块组织、无 HMR。

目标：迁移到 `frontend/` 源码目录 + Vite 6 多页打包 + Alpine.js 3 声明式状态管理 + Tailwind v4（`@tailwindcss/vite`），产物 `dist/` 由 `rust-embed` 编译期嵌入，**保持单文件自包含二进制、零外部 CDN、完全离线运行**的交付形态不变。

本次迁移**不改变后端行为**：WS action 集合、消息类型、认证机制、TLS、串口/AT 执行路径全部保持不变。前端以**真实后端契约**为准做 1:1 功能保真。

## 2. 现状约束（必须保持）

| 约束 | 来源 |
|---|---|
| 默认编译目标 `armv7-unknown-linux-musleabihf`，`rust-lld` 链接，无 ARM C 工具链 | `memory/project_build_target.md`、`.cargo/config.toml` |
| **所有依赖必须纯 Rust**，不得引入 `ring`/`openssl`/`cc` 等含 C 代码的 crate | `memory/cross_compile_pure_rust.md` |
| 单文件自包含二进制，部署为 `/opt/quectel-webui`，设备上无 Node/无外网 | `scripts/webui.sh` |
| 运行期不读文件系统提供静态资源（现状为编译期嵌入，迁移后仍如此） | `src/main.rs` 全部 `include_str!` |
| `templates/` 目录不关注、不修改 | `memory/ignore_templates.md` |
| Windows 开发环境：`build.rs` 需用 `npm.cmd` / `npx.cmd` | `memory/windows_rules.md` |

### 2.1 真实 WS 契约要点（迁移的事实来源）

- 入站信封：`{action: string, payload?: any}`。共 31 个 action（`manual_at`、`read_imei`、`write_imei`、`set_interval`、`set_view_state`、`get_static_info`、`get_backend_log`、`set_apn`、`set_network_mode`、`net_connect`、`net_disconnect`、`network_scan`、`send_sms`、`get_sms_list`、`get_device_info`、`reboot`、`factory_reset`、`flight_mode`、`set_band_lock`、`set_cell_lock`、`get_diagnostics`、`set_mode_pref`、`set_usb_net_mode`、`get_usb_config`、`set_sim_slot`、`get_mbn_list`、`set_mbn`、`mbn_autosel`、`mbn_deactivate`、`set_eth_config`、`set_ippt_config`）。
- 出站：**遥测用 `update_type`（`full`/`delta`）而非 `type`**，载荷嵌套在 `data` 下，共 28 个可空字符串字段；其余消息用 `type` 区分，载荷同样在 `data` 下。
- 一个请求对应**一个**终态响应，无流式中间进度。
- 未知 action、未知 diagnostics 子命令：**静默丢弃，不回任何消息**。
- **无 per-request 超时**：单个串口 actor 串行处理所有客户端请求。

## 3. 方案选型

| 方案 | 内容 | 结论 |
|---|---|---|
| **A：一次性完整迁移** | 新增 `frontend/` + Vite；`build.rs` 改调 `npm run build`；`main.rs` 换 `rust-embed`；删除旧前端文件。实施计划拆成多个可独立验证的提交 | **采用** |
| B：分层迁移 | 先只换构建链（前端仍原生 JS），再换 Alpine | 否决：前端要全量改两遍，总工作量更大 |
| C：最小改动 | 保留单文件，仅换 Tailwind 集成方式与 `rust-embed` | 否决：不解决命令式 DOM 冗长这一核心诉求 |

方案 A 通过"分步提交各自可验证"获得了方案 B 的验证纪律，同时没有 B 的重复劳动。

## 4. 目标目录结构

```
Cargo.toml             + rust-embed, mime_guess
build.rs               改为 npm run build(vite) → dist/；Node 缺失且 dist/ 已存在时降级复用
package.json           新增 alpinejs / vite / @tailwindcss/vite / tailwindcss；scripts.build = vite build
package-lock.json      纳入版本控制
vite.config.js         新增：root=frontend，多页 input(index+login)，outDir=../dist
.gitignore             取消 package.json / package-lock.json 忽略；dist/ 保持忽略
dist/                  Vite 输出（编译期被嵌入，不入库）

frontend/
  index.html                   主控台
  login.html                   登录页
  src/css/style.css            Tailwind v4 入口
  src/js/app.js                入口：注册各 store → Alpine.start()
  src/js/ws.js                 WS 单例：连接/指数退避重连/入站分发/请求超时
  src/js/contract.js           action 与消息类型常量、裸文本解析、出站载荷构造
  src/js/ui.js                 主题/全局 loading/断连遮罩/复制 toast/导航/页标题
  src/js/telemetry.js          遥测与硬件规格
  src/js/net.js                APN/选网/SA-NSA/网口/直通/IMEI/频段/小区/扫频/诊断/MBN/复位
  src/js/sms.js                收件箱/详情/发送
  src/js/usb.js                USB 模式与摘要
  src/js/logs.js               AT 控制台历史 + 后端环形日志
  src/js/login.js              登录页逻辑
  src/js/sha256.js             本地 SHA-256（crypto.subtle + 纯 JS 回退）

frontend/test/contract.test.js  node --test 纯函数测试（零新依赖）

src/main.rs            改用 rust-embed 托管静态资源；删除 include_str! 与 /style.css 路由
删除                    src/index.html、src/login.html、src/input.css
不动                    src/device.rs、src/at/*、src/bin/atcmd_rs.rs、scripts/*、templates/*
```

## 5. 构建链与静态托管

### 5.1 构建顺序

`build.rs` → `npm run build`（Vite）→ 产物写入 `dist/` → `rust-embed` 在编译 crate 时读 `dist/` 嵌入。`build.rs` 需 `rerun-if-changed` 覆盖 `frontend/`、`vite.config.js`、`package.json`。

`build.rs` 保留降级分支：npm 执行失败或 Node 不可用时，若 `dist/index.html` 已存在则警告并继续；若不存在则 panic 并给出中文提示（提示安装 Node 并执行 `npm install`）。Windows 使用 `npm.cmd`。

### 5.2 静态托管

```rust
#[derive(RustEmbed)]
#[folder = "dist/"]
struct Asset;
```

- 新增 `serve_embedded_file(path)`：从嵌入表取文件，`mime_guess` 定 MIME；`assets/*` 返回 `Cache-Control: public, max-age=31536000, immutable`，其余 `no-cache`；未命中返回 404。
- `/` 保留鉴权与重定向逻辑，HTML 来源改为 `dist/index.html`；`/login` 保留"已登录则跳 `/`"，来源改为 `dist/login.html`。
- 删除 `.route("/style.css", get(style_handler))` 与 `style_handler` 函数。
- 新增 `.fallback(static_handler)` 提供 `/assets/*` 等资源；该 handler 只查嵌入表，不访问文件系统。
- **不改变**：JWT 签发/校验、`auth_token` Cookie 属性、nonce 校验、`/ws` 的 401/403 与 Origin==Host 校验、TLS 初始化与 HTTP→HTTPS 301 重定向。

### 5.3 关于 `rust-embed` 的调试态行为

`rust-embed` 在 debug 构建下默认从文件系统读取 `dist/`（不嵌入），release 构建才真正嵌入。因此**验证自包含性必须在 release 构建上做**：把二进制拷到仓库外的临时目录（该目录无 `dist/`）运行，确认静态资源仍可访问。

## 6. WS 契约适配层（`contract.js`）

集中三件事，消灭字符串魔法：

1. **常量**：`ACTIONS`（31 个 action 名）、`TYPES`（`static_info`/`at_res`/`network_status`/`scan_result`/`sms_list`/`sms_sent`/`backend_log`/`device_info`/`band_lock_res`/`cell_lock_res`/`diagnostics_res`/`settings_log`/`usb_config_info`/`usb_net_res`/`sim_slot_res`/`mbn_list_res`/`mbn_set_res`/`imei_res`/`eth_res`/`ippt_res`）、`update_type` 取值。
2. **解析**：`parseAtResponse(text)`、`parseCmgl(text) -> [{sender, timestamp, text}]`（供 `at_res` 与 `sms_list` 复用）。
3. **出站载荷构造**：统一封装各 action 的载荷并锁定值类型 —— `flightMode(on)` → `"1"/"0"` 字符串；`mbnAutoSel(on)` → `"1"/"0"` 字符串；`setApn` 的 `auth` → 字符串；`setSimSlot` 的 `slot`、`setCellLock` 的 `pci`/`earfcn`/`band` → 数字；`setBandLock` 的 `nr5g` → 布尔。

### 6.1 契约修正清单

用户提供的参考实现与真实后端契约存在下列错配，实施时按"做法"列修正：

| # | 参考实现的假设 | 后端真实行为 | 做法 |
|---|---|---|---|
| 1 | `sms_list.data` 是 `{messages: []}` | 是**裸字符串**（原始 `+CMGL:` 文本） | 用 `parseCmgl` 解析；收件箱不再空白 |
| 2 | `device_info.rate` | 字段名是 `max_rate` | store 用 `maxRate` |
| 3 | 存在 `settings_info` 消息 | 后端**从不发送** | 不移植该死 handler；旧前端的同名死代码一并清除 |
| 4 | 用 `imei_res.kind === 'read'` 区分读写 | `kind` 在解析成功时**恒为 `"read"`**（写入成功也被判为 read） | 不依赖 `kind`，用 `ws.js` 中 pending 的**发起意图**（`intent: 'read'/'write'`）决定文案 |
| 5 | `simSlotStatusText` 未在状态对象声明 | 模板 `x-text` 绑定了它 | 在 `net.js` 显式声明为响应式字段 |
| 6 | 快捷 AT 板 7 条、USB 面板无快捷板 | 现有 UI 是 14 条 + USB `AT+QCFG="usbnet"` 快捷板 | 全部补回，避免功能丢失 |
| 7 | `resetBandLock()` 先本地清空磁贴 | —（纯前端行为） | 沿用 `bandResetPending`：等 `band_lock_res.success` 后才清空，失败还原 |
| 8 | `network_status` 只看 `data.msg` | 三种形状；`set_network_mode` 变体**没有 `status` 字段** | 统一按 `success` 判定，把真实结果写回 APN/拨号/选网状态栏 |
| 9 | 把 `update_type:"delta"` 当增量 | 服务端 tick 发的是**全量合并快照** | 明确按全量 merge 语义实现 `telemetry` |
| 10 | 直接构造裸 payload | 类型不符会**静默降级为默认值**且不回错误 | 一切出站经 `contract.js` 构造 |

另注：`usb_config_info` 的失败变体用 `error` 字段（其余消息用 `msg`）；`backend_log` 的 `data` 是**字符串数组**（每条已由后端格式化）；`at_res` 的 `data` 是裸字符串且含 CR/LF。

## 7. Alpine 状态模型与组件边界

7 个 store，各自单文件、单一职责：

| store | 状态 | 出站动作 |
|---|---|---|
| `ui` | `darkMode` `globalLoading` `loadingText` `disconnectModal` `copyToast` `currentTab` `mobileMenuOpen` `firmwareVersion` | 无（纯本地） |
| `telemetry` | 28 个遥测字段 + 派生百分比（`cpuPercent`/`memPercent`/`rsrqPercent`/`rsrpPercent`/`sinrPercent`）+ `activeSimSlot` + `deviceSpecs` + `specsRows`(getter) | `set_interval` `set_view_state` |
| `net` | `apn` `networkMode` `netStatusText` `eth` `ippt` `imei` `band` `cellLock` `scan` `diag` `mbn` `simSlotStatusText` `systemSettings` | 21 个 net/RF 动作 |
| `sms` | `messages` `selectedMsg` `recipient` `message` `statusText` | `get_sms_list` `send_sms` |
| `usb` | `mode` `applying` `statusText` `current{name,desc,num,updated}` | `set_usb_net_mode` `get_usb_config` |
| `logs` | `console{input,history}` `backend[]` | `manual_at` `get_backend_log` |
| `ws` | `connected` `reconnectAttempt` `pending`（socket 实例保持非响应式） | 传输层 |

**依赖方向严格单向**：模板 → store → `ws.js`。`ws.js` 不 import 任何 store；`app.js` 持有 `type → handler` 分发表，将入站消息投递给对应 store；store 之间不互相 import。

**规模约束**：单文件超过约 200 行即视为职责过载信号，`net.js` 若超出则按"配置类 / 射频类 / 诊断类"再拆。

**派生值**：一律用 getter（`bandLockPreview`、`specsRows`、百分比着色类），模板中不写复杂表达式；参考实现的 `x-for="[label,val] in [[…17 项…]]"` 内联数组每帧重建，改为 getter 返回稳定数组。

**主题**：沿用 `localStorage['theme']` 键与 `documentElement.classList` 的 `dark` 类，与 `login.js` 行为一致。

## 8. 请求生命周期（pending / 超时 / 反假成功）

后端存在"静默不回"与"无超时"两个硬事实，参考实现会让状态永久卡在 `⏳`。store 不直接 `ws.send`，统一走：

```js
ws.request(action, payload, { expect: TYPES.BAND_LOCK_RES, timeoutMs: 30000, intent: 'reset' })
```

1. 出站即登记 pending（`{action, expect, startedAt, intent}`），UI 显示"⏳ 进行中"（进行中，非成功断言）。
2. 同类操作只允许一个 in-flight，占用期间按钮 `disabled`。
3. 收到 `expect` 类型的消息 → 解除 pending、解锁按钮、按 `success` 渲染 ✓/✗。
4. 超时未回 → 显示 `✗ 操作超时（后端未返回）`、解锁按钮，**不改动任何用户输入与选中态**。
5. `success:false` → 显示 `✗ msg`，并**还原乐观 UI**（如频段磁贴回滚）。
6. **WS 断连 → 所有 pending 立即判失败并解锁**，杜绝状态悬挂。
7. 仅在后端确认成功后执行：清空短信正文、清空/锁定频段磁贴、写 ✓ 状态栏。

### 8.1 无 `success` 字段消息的处理规则

| 消息 | 判定规则 |
|---|---|
| `sms_sent`（只有 `status` 字符串） | `status` 含 `successfully` → 成功，才清空正文；否则保留正文并显示 ✗ |
| `at_res`（裸文本） | 只进 AT 控制台，不做成功断言 |
| `scan_result`（`status` + `networks`） | 关闭全局 loading、渲染表格；不做成功断言 |
| `settings_log` / `backend_log` | 后端驱动的日志，直接 append |
| `network_status`（三种形状） | 统一读 `success` 回写状态栏 |
| 遥测 `update_type` | 全量 merge 进 `telemetry`，不触发任何成功断言 |

点击即开全局 loading 的操作（扫频、诊断、MBN、USB 下发）在超时或失败路径上**必须关闭 loading**。

## 9. 错误处理

| 场景 | 处理 |
|---|---|
| WS 断开 | `ui.disconnectModal` 遮罩；指数退避 `min(60000, 3000*2^n) + 0~1000ms` 抖动重连；`onopen` 重置尝试计数并发 `get_static_info` |
| 页面可见性变化 | `visibilitychange` → `set_view_state 'active'/'idle'`（后端据此在 5s/15s 轮询间隔间切换） |
| 消息非 JSON | 捕获解析异常，写控制台，不断开连接 |
| 未识别 `type` | 忽略并记录（便于发现契约漂移），不抛错 |
| 用户输入非法 | 就地校验并提示（IMEI 必须 15 位数字、小区锁必填 PCI+EARFCN、APN 非空、短信收信人与正文非空），**不下发** |
| 操作失败 | 状态栏 ✗ + 保留输入；不做任何"已生效"断言 |

**契约漂移检测**：`app.js` 分发表在开发构建下对未注册的 `type` 打警告，便于后续后端新增消息时及时发现。

## 10. 测试与验收

1. **依赖纯净性**：`cargo tree --target armv7-unknown-linux-musleabihf` 确认 `rust-embed`、`mime_guess` 未引入 `cc`/`ring`/`openssl`。
2. **交叉编译**：`cargo build --release --target armv7-unknown-linux-musleabihf` 通过。
3. **自包含验证**：本机 release 二进制拷到仓库外临时目录（无 `dist/`）运行，确认 `/`、`/login`、`/assets/*` 均正常 —— 证明资源已真正嵌入而非读盘。
4. **静态托管 smoke**：未登录 `GET /` → 302 `/login`；登录后 `GET /` → 200；`/assets/*.js` → 200 且带 `immutable`；`GET /style.css` → 404（反向证明旧链路已移除）。
5. **契约单元测试**：`frontend/test/contract.test.js`，用 Node 内置 `node --test`（零新依赖），覆盖 `parseCmgl`（含多段、空文本、乱序）、`parseAtResponse`、以及各出站载荷的值类型断言。
6. **功能 1:1 验收清单**：逐面板打勾，重点覆盖 14 条快捷 AT、USB 快捷 AT 板、SMS 收件箱解析、IMEI 读写文案、USB 摘要由后端 `config` 驱动、频段重置确认后才清空。
7. **反假成功专项**：断网 / 拔卡 / 错误参数下逐一点击各操作按钮，确认无假 ✓、无永久 ⏳、用户输入不被吞。

## 11. 未变更的部分

- WS action 集合、消息类型与载荷结构（前端单向适配后端，不改后端契约）。
- 认证：nonce 生成/校验、`SHA256(nonce + username + SHA256(password))` 校验、JWT HS256 与 7 天有效期、`auth_token` Cookie 属性、`/ws` 的 Origin==Host 校验。
- TLS：`ENABLE_HTTPS`、`HTTP_REDIRECT`、`SSL_CERT_PATH`/`SSL_KEY_PATH`、`certkit` 内存自签证书、`rustls_rustcrypto` provider。
- 遥测轮询：`set_interval`、`active_views` 与空闲间隔、掉线合成快照。
- `build.rs` 的 Tailwind 调用被 Vite 取代，但"构建期生成前端产物 → 编译期嵌入"这一形态不变。

## 12. 文件变更清单

| 操作 | 路径 |
|---|---|
| 新增 | `vite.config.js`、`frontend/**`（含 `frontend/test/contract.test.js`） |
| 修改 | `Cargo.toml`、`build.rs`、`package.json`、`.gitignore`、`src/main.rs` |
| 入库 | `package-lock.json`（取消 `.gitignore` 忽略） |
| 删除 | `src/index.html`、`src/login.html`、`src/input.css` |
| 不动 | `src/device.rs`、`src/at/*`、`src/bin/atcmd_rs.rs`、`scripts/*`、`templates/*` |

中间态说明：本次为一次性切换，实施计划内部按"构建链 → 静态托管 → Alpine 骨架 → 各面板 → 清理旧文件"拆分提交，其中前几个提交落地时旧前端仍在，UI 短暂以旧文件为准；删除旧文件放在功能对齐确认之后。

## 13. 风险与对策

| 风险 | 对策 |
|---|---|
| `rust-embed` / `mime_guess` 隐含 C 依赖，破坏 armv7-musl 编译 | 第 10.1 项先行验证；若 `mime_guess` 引入 C 依赖，退化为手写"扩展名 → MIME"映射（仅需 html/css/js/svg/woff2 五种） |
| `dist/` 缺失导致编译失败（CI/新 clone） | `build.rs` 降级分支 + panic 中文提示；文档要求先 `npm install` |
| Tailwind v4 在 Vite 插件下扫描不到类名，样式大面积丢失 | 在 `frontend/src/css/style.css` 显式声明 `@source` 指向 `index.html` / `login.html`，不依赖自动探测 |
| debug 构建读盘、误判为"已嵌入" | 以 release + 移出仓库运行作为唯一判据（第 10.3 项） |
| 迁移中丢功能（曾发生：快捷 AT 从 14 条降到 7 条） | 第 10.6 项逐面板清单验收，不以"能打开页面"为完成标准 |
| 状态悬挂 / 假成功复发 | 第 8 节 pending 机制 + 第 10.7 项专项验收 |

## 14. 自审

- **占位符扫描**：无 TBD/TODO；第 6 节 action 与消息类型已逐一枚举。
- **内部一致性**：第 4 节目录结构与第 7 节 store 表一一对应；第 5.2 节删除 `/style.css` 与第 10.4 节 404 断言一致；第 8 节规则与第 6.1 节修正清单第 7、8 项一致。
- **范围**：单一实现计划可承载（构建链 + 一个前端包 + 静态托管替换），已拆成可独立验证的提交序列。
- **歧义**：明确"1:1 保真"以真实契约为准、"delta 按全量 merge"、"验证以 release 移出仓库为判据"。
