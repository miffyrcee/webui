# 新增 SIM 卡槽切换 + MBN 文件列表与手动选择

## 背景

当前 `quectel-webui` 只读不写卡槽：启动时通过 `AT+QUIMSLOT?` 读取当前槽位并展示在"活跃卡槽 Slot"（`info_sim`），但无法切换。MBN 方面已有 `DiagnosticType::MbnList`（`AT+QMBNCFG="List"`）和" MBN 分配"按钮，但只显示原始文本，无法从列表中手动选择 MBN 并下发。

目标：新增两个功能——
1. **切换 SIM 卡槽**：前端在"活跃卡槽 Slot"旁提供 SIM1/SIM2 切换，后端只发 `AT+QUIMSLOT=<slot>`，**不自动重启**（生效由用户手动处理）。
2. **MBN 列表查询 + 手动选择**：前端提供下拉列表，枚举模组可用 MBN（`AT+QMBNCFG="List"`），用户选中后下发 `AT+QMBNCFG="Select","<name>"`，选择成功后提示需重启生效。

## 方案选型

- **卡槽**：新增独立 `HardwareBackend` trait 方法 `set_sim_slot`，与现有 `set_band_lock`/`set_cell_lock` 模式一致，职责清晰、可 Mock、可测试。
- **MBN**：从 `DiagnosticType` 诊断通道拆出，独立 trait 方法 + 结构化返回，前端直接渲染下拉；原始" MBN 分配"按钮保留。

## 后端设计

### 改动文件

- `src/main.rs` — `AtAction`、`HardwareBackend` trait、`RealBackend`、`MockBackend`、`handle_at_request`、WS handler
- `src/at/response.rs` — 新增 `QmbncfgEntry`
- `src/at/parser.rs` — 新增 `ParsedLine::Qmbncfg` 变体与解析

### A. 切换卡槽

| 层 | 改动 |
|---|---|
| `AtAction` | 新增 `SetSimSlot(u32)` |
| `HardwareBackend` | 新增 `async fn set_sim_slot(&self, slot: u32) -> Result<String, String>` |
| `RealBackend` | 入口校验 `slot ∈ {1,2}`，否则 `Err("无效的 SIM 卡槽，仅支持 1/2")`；合法则 `AT+QUIMSLOT=<slot>`（走统一 `send_at_command_inner`） |
| `MockBackend` | 模拟返回 `OK`，记 mock 日志 |
| `handle_at_request` | `SetSimSlot(slot)` 分支：调用后返回 JSON `{ success, slot, msg, note: "切换卡槽后需重启模组方可生效" }` |
| WS | 新增 `"set_sim_slot"` action，payload `{ slot: u32 }` |

### B. MBN 列表 + 手动选择

| 层 | 改动 |
|---|---|
| `response.rs` | `QmbncfgEntry { index: u32, state: u32, name: String }`，`state=1` 表示当前激活 |
| `parser.rs` | `ParsedLine::Qmbncfg(QmbncfgEntry)`；解析 `+QMBNCFG: "List",<index>,<state>,"<name>"`，仅认 `"List"` 类型行，`name` 为空忽略 |
| `AtAction` | 新增 `GetMbnList`、`SetMbn(String)` |
| `HardwareBackend` | 新增 `async fn get_mbn_list(&self) -> Result<Vec<serde_json::Value>, String>`、`async fn set_mbn(&self, name: &str) -> Result<String, String>` |
| `RealBackend` | `get_mbn_list`: `AT+QMBNCFG="List"` → 逐行 `parse_single_line` 过滤 `Qmbncfg` 且 name 非空 → 组装 `{ index, state, name }`；列表为空则 `Err`。`set_mbn`: name 经 `sanitize_at_param` → `AT+QMBNCFG="Select","<name>"` |
| `MockBackend` | 返回几条固定假 MBN（含一条 `state=1`），`set_mbn` 模拟成功 |
| `handle_at_request` | `GetMbnList` → `{ success, list }`；`SetMbn` → `{ success, msg, note: "选择 MBN 后需重启模组方可生效" }` |
| WS | 新增 `"get_mbn_list"`、`"set_mbn"` 两个 action |

### 错误处理

- 非法卡槽（非 1/2）→ 后端 `Err`，前端显示错误。
- MBN 列表 AT 返回 ERROR 或解析为空 → 后端 `Err` 透传；选择失败 → 透传错误。
- 所有新参数（MBN 名）经 `sanitize_at_param` 防 AT 注入。

## 前端设计（`src/index.html`）

### A. 卡槽切换 UI

- **位置**：现有"活跃卡槽 Slot"卡片（`info_sim`）内部，数值下方。
- 新增：`SIM 1` / `SIM 2` 两个按钮 + 状态提示 `sim_slot_status`。
- 点击按钮 → `sendWsCommand('set_sim_slot', { slot: 1|2 })`。
- 收到 `sim_slot_res` 成功 → 更新 `info_sim` 为 `SIM <slot>`、高亮对应按钮、显示"需重启生效"提示；失败 → 显示错误。

### B. MBN 选择 UI

- **位置**：诊断区（"MBN 分配"/"AutoSel MBN" 按钮行下方），保留原按钮。
- 新增一行：`<select id="mbn_select">` 下拉 + `刷新列表` 按钮 + `应用所选 MBN` 按钮 + 状态提示 `mbn_status`。
- 刷新 → `get_mbn_list` → 填充下拉，激活项（`state=1`）标注"（当前）"并默认选中。
- 应用 → `set_mbn {name}` → 显示结果 + "需重启生效"提示。

### C. WS 消息处理

- `WS_HANDLERS` 新增：`'sim_slot_res'` → `handleSimSlotRes`、`'mbn_list_res'` → `handleMbnListRes`、`'mbn_set_res'` → `handleMbnSetRes`。
- 新增 `initSimSlotPanel()`、`initMbnPanel()`，并在初始化调用链追加。

### D. 交互数据流

```
点 SIM2 → WS set_sim_slot → actor → AT+QUIMSLOT=2 → sim_slot_res
        → info_sim="SIM 2" + 按钮高亮 + "需重启生效"提示

点刷新 → WS get_mbn_list → actor → AT+QMBNCFG="List" → mbn_list_res → 填充下拉
点应用 → WS set_mbn → actor → AT+QMBNCFG="Select","xxx" → mbn_set_res → 结果+重启提示
```

## 测试

- `parser.rs` 单元测试：`+QMBNCFG: "List",0,1,"RM520NGLAAR01A02M4G_01.004"` 正确解析；非 `"List"` 行忽略；空 name 行忽略。
- `MockBackend` 下可手动验证前端两个功能的完整交互（无真机环境）。
- 现有测试保持通过。
