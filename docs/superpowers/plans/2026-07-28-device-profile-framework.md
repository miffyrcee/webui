# DeviceProfile 多模组适配框架 — 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增 `src/device.rs` 设备 Profile 框架 + 修改 `RealBackend` 使其自动检测模组型号并读取 profile 数据，同时将 3 处硬编码改为 profile 驱动，并添加 `device_name()` 到 `HardwareBackend` trait。

**Architecture:** 新增 `src/device.rs` 模块定义 `DeviceId` 枚举和 `DeviceProfile` 结构体，静态注册表存放已知设备 profile。`RealBackend` 新增 `profile: &'static DeviceProfile` 字段，通过 `async fn new()` 构造器在启动时自动检测。`set_band_lock` 和 `set_cell_lock` 中的硬编码值改为从 `self.profile` 读取。`HardwareBackend` trait 新增 `device_name()` 方法。

**Tech Stack:** Rust, no new dependencies.

## Global Constraints

- 纯 Rust 实现，无新依赖（保持 armv7-musl 交叉编译兼容）
- 遵循现有代码风格：`edition = "2024"`, 中文日志前缀
- 新增设备 profile 只需加一条 static 记录 + 注册到 `ALL_PROFILES` + 加一个测试
- `DeviceProfile` 所有字段 `pub`，`DeviceId` 派生 `PartialEq, Eq`
- 未知模组无缝降级到 `PROFILE_GENERIC`

---

### Task 1: 创建 `src/device.rs` — DeviceProfile 核心类型 + 预设 Profile + 查找函数 + 单元测试

**Files:**
- Create: `src/device.rs`
- Modify: `src/main.rs` — 在顶部加 `mod device;`
- Test: 内联在 `src/device.rs` 的 `#[cfg(test)] mod tests { ... }`

**Interfaces:**
- Produces (供 Task 2/3/4 消费):
  - `pub enum DeviceId { Rm520n, Rm502q, Generic }`
  - `pub struct DeviceProfile { pub id, pub name, pub cgmm_prefixes, pub default_nr_bands, pub default_lte_bands, pub nr_cell_lock_scs_threshold, pub has_eth_driver }`
  - `impl DeviceProfile { pub fn matches(&self, cgmm_response: &str) -> bool }`
  - `pub static PROFILE_RM520N: DeviceProfile`
  - `pub static PROFILE_RM502Q: DeviceProfile`
  - `pub static PROFILE_GENERIC: DeviceProfile`
  - `pub static ALL_PROFILES: &[&'static DeviceProfile]`
  - `pub fn lookup_profile(cgmm_response: &str) -> &'static DeviceProfile`

- [ ] **Step 1: 创建 `src/device.rs`，编写全部代码**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceId {
    Rm520n,
    Rm502q,
    Generic,
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceProfile {
    pub id: DeviceId,
    pub name: &'static str,
    pub cgmm_prefixes: &'static [&'static str],
    pub default_nr_bands: &'static str,
    pub default_lte_bands: &'static str,
    pub nr_cell_lock_scs_threshold: u32,
    pub has_eth_driver: bool,
}

impl DeviceProfile {
    pub fn matches(&self, cgmm_response: &str) -> bool {
        let clean = cgmm_response.trim().to_uppercase();
        self.cgmm_prefixes
            .iter()
            .any(|prefix| clean.contains(&prefix.to_uppercase()))
    }
}

// ---------------------------------------------------------------------------
// 静态设备注册表
// ---------------------------------------------------------------------------

/// RM520N / RG520N 系列 (高通 SDX62, 3GPP Rel-16)
pub static PROFILE_RM520N: DeviceProfile = DeviceProfile {
    id: DeviceId::Rm520n,
    name: "RM520N / RG520N Series",
    cgmm_prefixes: &["RM520N", "RG520N"],
    default_nr_bands: "1:2:3:5:7:8:12:13:14:18:20:25:26:28:29:30:38:40:41:48:66:70:71:75:76:77:78:79",
    default_lte_bands: "1:2:3:4:5:7:8:12:13:14:17:18:19:20:25:26:28:29:30:32:34:38:39:40:41:42:43:48:66:71",
    nr_cell_lock_scs_threshold: 28,
    has_eth_driver: true,
};

/// RM502Q-AE (高通 SDX55, 3GPP Rel-15)
pub static PROFILE_RM502Q: DeviceProfile = DeviceProfile {
    id: DeviceId::Rm502q,
    name: "RM502Q-AE",
    cgmm_prefixes: &["RM502Q"],
    default_nr_bands: "1:2:3:5:7:8:12:20:28:38:40:41:48:66:71:77:78:79",
    default_lte_bands: "1:2:3:4:5:7:8:12:13:14:17:18:19:20:25:26:28:29:30:32:34:38:39:40:41:42:43:46:48:66:71",
    nr_cell_lock_scs_threshold: 28,
    has_eth_driver: false,
};

/// 通用/降级 Quectel Profile (未知模组时使用)
pub static PROFILE_GENERIC: DeviceProfile = DeviceProfile {
    id: DeviceId::Generic,
    name: "Generic Quectel Module",
    cgmm_prefixes: &[],
    default_nr_bands: "1:3:8:28:41:77:78:79",
    default_lte_bands: "1:3:5:8:34:38:39:40:41",
    nr_cell_lock_scs_threshold: 28,
    has_eth_driver: false,
};

/// 包含所有已知设备的列表（查找顺序：先匹配先返回）
pub static ALL_PROFILES: &[&'static DeviceProfile] = &[
    &PROFILE_RM520N,
    &PROFILE_RM502Q,
];

/// 根据 AT+CGMM 返回值查找匹配的 DeviceProfile
pub fn lookup_profile(cgmm_response: &str) -> &'static DeviceProfile {
    ALL_PROFILES
        .iter()
        .find(|p| p.matches(cgmm_response))
        .copied()
        .unwrap_or(&PROFILE_GENERIC)
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_matching_rm520n_gl() {
        let p = lookup_profile("RM520N-GL");
        assert_eq!(p.id, DeviceId::Rm520n);
    }

    #[test]
    fn test_profile_matching_rg520n() {
        let p = lookup_profile("RG520NGLAA-m4g");
        assert_eq!(p.id, DeviceId::Rm520n);
    }

    #[test]
    fn test_profile_matching_rm502q_ae() {
        let p = lookup_profile("RM502Q-AE");
        assert_eq!(p.id, DeviceId::Rm502q);
    }

    #[test]
    fn test_profile_matching_rm502q_full() {
        let p = lookup_profile("RM502QAEAR11A02M4G");
        assert_eq!(p.id, DeviceId::Rm502q);
    }

    #[test]
    fn test_profile_matching_fallback() {
        let p = lookup_profile("UNKNOWN_MODULE");
        assert_eq!(p.id, DeviceId::Generic);
    }

    #[test]
    fn test_scs_threshold_rm502q() {
        let p = lookup_profile("RM502Q-AE");
        assert_eq!(p.nr_cell_lock_scs_threshold, 28);
    }
}
```

- [ ] **Step 2: 在 `src/main.rs` 顶部注册新模块**

在 `mod at;` 之后增加一行：
```rust
mod at;
mod device;
```

- [ ] **Step 3: 运行单元测试确认通过**

```bash
cargo test -- test_profile 2>&1
```

Expected: 全部 6 个测试 PASS。

- [ ] **Step 4: Commit**

```bash
git add src/device.rs src/main.rs
git commit -m "feat: 新增 DeviceProfile 框架，支持模组自动检测与 Profile 查找

- 新增 src/device.rs：DeviceId 枚举、DeviceProfile 结构体、静态注册表
- 预设 RM520N/RG520N、RM502Q-AE、Generic Quectel 三个 Profile
- lookup_profile() 支持 CGMM 前缀匹配，未知模组降级到 Generic
- 6 个单元测试覆盖所有匹配场景"
```

---

### Task 2: RealBackend 集成 DeviceProfile — 新增 `profile` 字段 + `async fn new()` 构造器

**Files:**
- Modify: `src/main.rs` 中 `RealBackend` 结构体和 impl 块

**Interfaces:**
- Consumes: `device::lookup_profile`, `device::PROFILE_GENERIC`
- Produces: `RealBackend { serial_path: String, profile: &'static DeviceProfile }`
- Produces: `RealBackend::async fn new(serial_path: String) -> Self`

- [ ] **Step 1: 修改 `RealBackend` 结构体**

当前：
```rust
struct RealBackend {
    serial_path: String,
}
```

改为：
```rust
struct RealBackend {
    serial_path: String,
    profile: &'static device::DeviceProfile,
}
```

- [ ] **Step 2: 实现 `async fn new()` 构造器**

在 `impl RealBackend {` 块的开头（目前硬 `impl HardwareBackend for RealBackend` 前），新增一个 `impl RealBackend` 块：

```rust
impl RealBackend {
    async fn new(serial_path: String) -> Self {
        let mut backend = Self {
            serial_path,
            profile: &device::PROFILE_GENERIC,
        };
        // 必须使用 std::path 再次检查，因为 main() 中构造前可能路径已不存在
        if std::path::Path::new(&backend.serial_path).exists() {
            if let Some(cgmm) = send_at_get_line(&backend.serial_path, "AT+CGMM").await {
                let detected = device::lookup_profile(&cgmm);
                push_log("INFO", "Device", &format!(
                    "检测到模组: {} → Profile: {}",
                    cgmm.trim(),
                    detected.name,
                ));
                backend.profile = detected;
            } else {
                push_log("WARN", "Device", "AT+CGMM 无响应，使用通用 Quectel Profile");
            }
        } else {
            push_log("WARN", "Device", "串口设备不存在，使用通用 Quectel Profile");
        }
        backend
    }
}
```

- [ ] **Step 3: 修改 `main()` 中 RealBackend 的构造方式**

当前：
```rust
Arc::new(RealBackend { serial_path: serial_path.clone() })
```

改为：
```rust
Arc::new(RealBackend::new(serial_path.clone()).await)
```

并在 `main()` 前面增加 `use device::DeviceProfile;` 导入（如果后续需要直接访问 profile）。

- [ ] **Step 4: 确认编译通过**

```bash
cargo check 2>&1
```

Expected: 编译成功，无警告。

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "refactor: RealBackend 集成 DeviceProfile，启动时通过 AT+CGMM 自动检测模组

- RealBackend 新增 profile 字段
- async fn new() 构造器在启动时发送 AT+CGMM 并匹配 profile
- 检测失败时优雅降级到 PROFILE_GENERIC"
```

---

### Task 3: 将 `set_band_lock` 和 `set_cell_lock` 中的硬编码替换为 profile 驱动

**Files:**
- Modify: `src/main.rs` 中 `set_band_lock`（行 883-900）和 `set_cell_lock`（行 902-917）方法

- [ ] **Step 1: 修改 `set_band_lock`**

当前（行 883-900）：
```rust
async fn set_band_lock(&self, is_nr5g: bool, bands: &str) -> Result<String, String> {
    let cmd = if is_nr5g {
        let b = if bands.is_empty() || bands == "all" {
            "1:2:3:5:7:8:12:20:25:28:38:40:41:48:66:71:77:78:79"
        } else {
            bands
        };
        format!("AT+QNWPREFCFG=\"nr5g_band\",{}", b)
    } else {
        let b = if bands.is_empty() || bands == "all" {
            "1:3:5:8:34:38:39:40:41"
        } else {
            bands
        };
        format!("AT+QNWPREFCFG=\"lte_band\",{}", b)
    };
    send_at_command_inner(&self.serial_path, &cmd).await
}
```

改为：
```rust
async fn set_band_lock(&self, is_nr5g: bool, bands: &str) -> Result<String, String> {
    let cmd = if is_nr5g {
        let b = if bands.is_empty() || bands == "all" {
            self.profile.default_nr_bands
        } else {
            bands
        };
        format!("AT+QNWPREFCFG=\"nr5g_band\",{}", b)
    } else {
        let b = if bands.is_empty() || bands == "all" {
            self.profile.default_lte_bands
        } else {
            bands
        };
        format!("AT+QNWPREFCFG=\"lte_band\",{}", b)
    };
    send_at_command_inner(&self.serial_path, &cmd).await
}
```

- [ ] **Step 2: 修改 `set_cell_lock` 中的 SCS 阈值**

当前（行 912）：
```rust
let scs = if b <= 28 { 15 } else { 30 };
```

改为：
```rust
let scs = if b <= self.profile.nr_cell_lock_scs_threshold { 15 } else { 30 };
```

- [ ] **Step 3: 确认编译通过**

```bash
cargo check 2>&1
```

Expected: 编译成功，无警告。

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "refactor: set_band_lock 和 set_cell_lock 的硬编码改为 profile 驱动

- NR/LTE "all" 频段从 self.profile.default_nr_bands / default_lte_bands 读取
- 5G SCS 分界阈值从 self.profile.nr_cell_lock_scs_threshold 读取
- RM502Q-AE 的 NR 频段不含 Rel-16 特有频段 (n13/n14/n18/n25/n26/n29/n30/n70/n75/n76)"
```

---

### Task 4: 为 `HardwareBackend` trait 新增 `device_name()` 方法 + 更新启动日志

**Files:**
- Modify: `src/main.rs` — trait 定义、RealBackend impl、MockBackend impl、main() 启动日志

- [ ] **Step 1: 在 `HardwareBackend` trait 中新增 `device_name()`**

当前（行 621-641）：
```rust
#[async_trait::async_trait]
trait HardwareBackend: Send + Sync {
    async fn exec_raw_at(&self, cmd: &str) -> String;
    // ... 现有方法
    async fn read_static_info(&self) -> (String, String, String, String);
}
```

在 `read_static_info` 之后增加：
```rust
    fn device_name(&self) -> &str;
```

- [ ] **Step 2: 在 `RealBackend` impl 中实现**

在 `RealBackend` 的 `impl HardwareBackend for RealBackend` 块中，任意位置新增：
```rust
    fn device_name(&self) -> &str {
        self.profile.name
    }
```

- [ ] **Step 3: 在 `MockBackend` impl 中实现**

在 `MockBackend` 的对应 trait impl 块中新增：
```rust
    fn device_name(&self) -> &str {
        "Mock Backend (Testing)"
    }
```

- [ ] **Step 4: 更新 `main()` 中的启动日志**

当前（行 2098 和行 2124）：
```rust
push_log("INFO", "System", "检测到真实串口设备，使用 RealBackend");
// ...
push_log("INFO", "System", "RM520N WebUI 后端服务已在 http://0.0.0.0:3000 监听");
```

改为：
```rust
let device_name = backend.device_name();
push_log("INFO", "System", &format!("检测到设备: {}", device_name));
// ...
push_log("INFO", "System", &format!("{} WebUI 后端服务已在 http://0.0.0.0:3000 监听", device_name));
```

注意：`main()` 中 `backend` 是 `Arc<dyn HardwareBackend>`，直接调用 `backend.device_name()` 即可（方法返回 `&str`，生命周期允许）。

- [ ] **Step 5: 确认编译通过**

```bash
cargo check 2>&1
```

Expected: 编译成功，无警告。

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat: HardwareBackend trait 新增 device_name() + 启动日志使用设备名

- RealBackend 返回 profile.name
- MockBackend 返回固定标识
- 启动日志输出动态设备名取代硬编码 'RM520N'"
```

---

## Self-Review

**Spec coverage:**
- DeviceProfile 核心类型 ✅ (Task 1)
- 预设 Profile（RM520N, RM502Q, Generic）✅ (Task 1)
- 自动检测（async new()）✅ (Task 2)
- set_band_lock 硬编码替换 ✅ (Task 3)
- set_cell_lock SCS 替换 ✅ (Task 3)
- HardwareBackend::device_name() ✅ (Task 4)
- 启动日志动态化 ✅ (Task 4)
- 单元测试覆盖匹配逻辑 ✅ (Task 1)

**Placeholder scan:** 无 TBD/TODO/占位符。所有代码块完整。

**Type consistency:** `lookup_profile` 返回 `&'static DeviceProfile`，`PROFILE_GENERIC` 是 `DeviceProfile`。`RealBackend.profile` 类型是 `&'static DeviceProfile`。`device_name()` 返回 `&str`。所有签名跨任务一致。
