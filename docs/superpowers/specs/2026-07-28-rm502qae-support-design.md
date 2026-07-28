# 多模组适配：新增 RM502Q-AE 支持与 DeviceProfile 扩展框架

## 背景

当前 `quectel-webui` 仅针对一款 Quectel 5G 模组（RM520N-GL/RG520N-GL, 高通 SDX62 平台），大量设备特定数据（频段列表、锁小区参数等）硬编码在 `RealBackend` 实现中。需要新增对 RM502Q-AE 模组（高通 SDX55 平台）的支持，并建立可扩展的框架以便未来添加更多模组。

## 模组差异概要

| 维度 | RM502Q-AE (SDX55, Rel-15) | RG520N-GL (SDX62, Rel-16) |
|------|---------------------------|---------------------------|
| 5G NR 频段 | n1/n2/n3/n5/n7/n8/n12/n20/n28/n38/n40/n41/n48/n66/n71/n77/n78/n79 | 新增 n13/n14/n18/n25/n26/n29/n30/n70/n75/n76 |
| LTE 频段 | 多 B46 (LAA) | 核心集一致 |
| USB 网络模式 | 编码完全一致 (0=RMNET, 1=ECM, 2=MBIM, 3=RNDIS, 5=NCM) | 同左 |
| AT 指令兼容性 | ~95% 兼容，不支持 AT+QETH 和部分 Rel-16 指令 | 完整支持 |
| 固件前缀 | `RM502QAE...` | `RG520NGL...` / `RM520NGL...` |
| 串口路径 | `/dev/smd11` | `/dev/smd11` |

## 设计：DeviceProfile 框架

### 核心结构体

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceId {
    Rm520n,
    Rm502q,
    Generic,
}

pub struct DeviceProfile {
    pub id: DeviceId,
    pub name: &'static str,
    pub cgmm_prefixes: &'static [&'static str],
    pub default_nr_bands: &'static str,
    pub default_lte_bands: &'static str,
    pub nr_cell_lock_scs_threshold: u32,
    pub has_eth_driver: bool,
}
```

### 预设 Profile 注册表（`src/device.rs`）

- `PROFILE_RM520N` — SDX62 平台（RM520N/RG520N 系列）
- `PROFILE_RM502Q` — SDX55 平台（RM502Q-AE）
- `PROFILE_GENERIC` — 未知模组的降级配置
- `ALL_PROFILES` — 顺序查找的注册表数组

### 自动检测

采用 `RealBackend::async fn new()` 构造器模式，启动时发送 `AT+CGMM` 检测模组型号，匹配注册表后绑定对应 Profile。检测失败则降级到 `PROFILE_GENERIC`。

```rust
impl RealBackend {
    pub async fn new(serial_path: String) -> Self {
        let mut backend = Self {
            serial_path,
            profile: &PROFILE_GENERIC,
        };
        if let Some(cgmm) = send_at_get_line(&backend.serial_path, "AT+CGMM").await {
            let detected = lookup_profile(&cgmm);
            backend.profile = detected;
        }
        backend
    }
}
```

### 受影响的现有方法（共 3 处）

| 方法 | 当前硬编码 | 改为 |
|------|-----------|------|
| `set_band_lock` NR "all" | 固定频段字符串 | `self.profile.default_nr_bands` |
| `set_band_lock` LTE "all" | 固定频段字符串 | `self.profile.default_lte_bands` |
| `set_cell_lock` 5G SCS | `if b <= 28` | `if b <= self.profile.nr_cell_lock_scs_threshold` |

未受影响的方法（指令完全兼容）：USB 模式、网络模式切换、诊断、APN、短信、拨号、遥测轮询。

### HardwareBackend trait 扩展

增加 `device_name()` 方法用于启动日志和设备标识：

```rust
#[async_trait]
trait HardwareBackend: Send + Sync {
    fn device_name(&self) -> &str;
    // ... 现有方法不变
}
```

`RealBackend` 返回 `self.profile.name`，`MockBackend` 返回固定字符串。

## 扩展新设备的操作路径

**三步走**（开闭原则）：

1. **添加 Profile 声明**（~12 行）— 在 `src/device.rs` 中定义静态 Profile
2. **注册到 `ALL_PROFILES`**（1 行）— 查到即可自动识别
3. **添加单元测试**（~5 行）— 验证自动匹配逻辑

对于私有指令分支，使用 `DeviceId` 枚举做编译期类型安全匹配：

```rust
match profile.id {
    DeviceId::Rm510q => self.send_special_cmd().await?,
    _ => self.send_standard_cmd().await?,
}
```

## 未变更的架构设计

- 后端选择逻辑不变（串口存在 → RealBackend，否则 → MockBackend）
- AT 命令基础设施（`send_at_command_inner`, `spawn_atcmd_rs`）不变
- `hardware_task` 串行 Actor 模型不变
- WebSocket 通信协议不变
- 前端代码不变
- 交叉编译目标和依赖不变

## 文件变更清单

| 文件 | 操作 | 说明 |
|------|------|------|
| `src/device.rs` | **新增** | DeviceProfile 结构体、预设 Profile、查找函数、单元测试 |
| `src/main.rs` | 修改 | RealBackend::new() 构造器；3 处硬编码改为 profile 调用；启动日志；HardwareBackend trait 加 device_name() |
| `src/at/mod.rs` | 不变 | |
| `src/at/parser.rs` | 不变 | |
| `src/at/response.rs` | 不变 | |
| `Cargo.toml` | 不变 | |
