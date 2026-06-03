# Cline AI Change History

## 2026-06-04 00:06 — 新增 6 个 WebUI 选项卡 (Simple Network / Simple Scan / Simple Settings / SMS / Console / Device Information)
- **修改内容**:
  - `src/index.html`: 重构为 7 选项卡布局 (Dashboard + 6 new)
    - **Simple Network**: APN 配置表 (apn/user/pass/auth type)、网络模式选择 (auto/5G/LTE/5G+LTE/WCDMA)、Connect/Disconnect 按钮、Connection Info 面板
    - **Simple Scan**: 网络扫描结果表格 (#号/运营商/MCCMNC/技术/状态/频段)，Start Scan 按钮触发 mock 扫描结果返回 3 个网络
    - **Simple Settings**: 轮询间隔设置、串口端口显示、WebSocket 状态、活跃视图计数、Reboot/Factory Reset/Flight Mode 按钮、操作日志区
    - **SMS**: 收件箱列表 (含 Refresh)、发送 SMS 表单 (收件人/消息/字数统计 0-160)
    - **Console**: 原 AT Terminal 迁移至此，保留 AT 命令输入和快捷命令
    - **Device Information**: 设备标识 (制造商/型号/固件/IMEI/序列号/硬件版本/模块类型)、SIM & 网络 (SIM状态/IMSI/ICCID/电话号码/网络状态/信号/温度)、能力信息 (频段/最大速率/VoLTE/GNSS)、Refresh 按钮
    - Tab 导航改为通用循环逻辑 (`allTabs` 数组)，支持 overflow-x-auto 水平滚动
  - `src/main.rs`: WebSocket 后端扩展
    - `WsCommand.payload` 类型从 `Option<String>` 改为 `Option<serde_json::Value>` 支持 JSON 对象负载
    - `set_interval` 解析兼容 Number 和 String 两种形式
    - 新增 12 个 WS action handler (全部通过 local_tx 直接回复):
      - `set_apn`: 解析 apn/user/pass/auth JSON 对象
      - `set_network_mode`: 网络模式设置
      - `net_connect` / `net_disconnect`: 连接控制
      - `network_scan`: 返回 mock 扫描结果 3 个网络
      - `send_sms`: 解析 recipient/message JSON 对象
      - `get_sms_list`: 返回 mock SMS 列表 2 条
      - `get_device_info`: 返回 mock 设备完整信息
      - `reboot` / `factory_reset` / `flight_mode`: 设备操作
- **涉及文件**:
  - `src/index.html`
  - `src/main.rs`
  - `CLINE_HISTORY.md`

## 2026-05-29 01:44 — 修复固件版本获取超时：SMD 通道初始化握手
- **问题**: `fetch_static_info()` 中 `drain_serial_buffer()` 被注释掉，且缺少 ATE0 关闭回显和 AT 验证通信。对于 SMD 设备 `/dev/smd11`，开机日志或前次会话残留数据堵塞缓冲区，导致 `AT+CGMR` 的响应与预期格式不匹配，`send_at_command_async()` 读循环无法找到 `\r\nOK\r\n` 终止符，直到 10 秒 IO_TIMEOUT 触发才返回 `"读超时(10s)"`
- **修改内容**:
  - 恢复 `fetch_static_info()` 中 `drain_serial_buffer()` 的调用，清空 SMD 通道残留数据
  - 在 `fetch_static_info()` 的 `AT+CGMR` 之前增加 `ATE0`（关闭回显）+ `AT`（验证通信）的初始化握手
  - 在 `hardware_polling_actor()` 中也增加同样的初始化握手（drain + ATE0 + AT）
  - 新增 `read_with_short_timeout()` 函数，使用 300ms 短超时读取（不依赖 `IO_TIMEOUT` 的 10s），用于 `drain_serial_buffer()` 实现非阻塞清空
  - 修复上一轮编辑引入的 `打印` 中文残留字符
- **涉及文件**:
  - `src/main.rs`
  - `CLINE_HISTORY.md`

## 2026-05-29 02:00 — 实机测试确认：ATE0 写入永久阻塞，问题未解决
- **问题**: 尽管新增了 500ms 串口 read 超时和 5s 总超时，程序在 `/dev/smd11` 上依然卡死，无任何错误输出
- **诊断结果**: 卡死点位于 `send_at_command_async("ATE0")` 的 `write_all()` 写入操作，而非 read 阶段。`write_all()` 在 SMD 通道 `/dev/smd11` 上永久阻塞，不返回错误、不触发超时
- **尝试修改内容**:
  - 串口 read 超时从 10s → 500ms（可及时检测读卡死）
  - 新增 AT 命令 5s 总超时截止（可及时检测写卡死）
  - 全流程新增诊断日志（定位卡死准确阶段）
- **结果**: 均无效。SMD 通道 `write()` 阻塞可能是内核级问题（驱动无响应/流控挂死/设备未就绪），无法通过应用层超时解决
- **建议下一步**:
  - 回到 `/dev/ttyIN`（socat PTY 桥接），该通道已被 `adb shell "echo AT > /dev/ttyIN && ... cat /dev/ttyOUT"` 确认正常
  - 或排查 `/dev/smd11` 是否被其他进程独占（`fuser /dev/smd11`）
- **涉及文件**:
  - `src/main.rs`
  - `CLINE_HISTORY.md`

## 2026-05-29 01:32 — 移除 tokio-serial 依赖
- **修改内容**: 完全移除 `tokio-serial` crate 的使用
  - 删除 `Cargo.toml` 中的 `tokio-serial = "5.4.5"` 依赖
  - 删除 `SerialHandle::Tty` 变体，只保留 `Raw(tokio::fs::File)`
  - 删除 `skip_tty_mode()` 函数
  - 简化 `open_serial()` 只使用 `tokio::fs::File` 直接打开设备（SMD/socat PTY 等非TTY设备）
  - 删除 `drain_serial_buffer()`、`send_at_command_async()` 中所有 Tty 模式匹配分支
  - `send_at_command_async()` 中 SMD 200ms 延迟始终执行（不再条件判断）
- **原因**: 实际设备使用 `/dev/smd11`（SMD 通道）或 socat PTY 桥接设备（`/dev/ttyIN`），都不是标准 TTY 串口。`tokio_serial` 的 `tcsetattr` 反而会破坏 socat 的 raw 配置，且 SMD 通道也不支持 TTY ioctl。所有设备都通过 Raw 文件模式工作正常
- **涉及文件**:
  - `Cargo.toml`
  - `src/main.rs`

## 2026-05-29 01:27 — 验证 /dev/smd11 SMD 通道直连正常
- **验证内容**: `cat /dev/smd11` 测试确认 `/dev/smd11` 为可直接 AT 通信的 SMD 通道
- **结果**: ATI 正确返回 Quectel RG520N-EU (Revision: RG520NEUDCR01A02M4G_TP)
- **说明**: 代码已配置 `/dev/smd11` 为默认串口，且 `open_serial()` 的 TTY→Raw 回退策略正确处理 SMD 设备。无需额外修改
- **涉及文件**:
  - 无需代码修改（仅本文档记录）

## 2026-05-29 01:16 — 修改默认串口设备为 /dev/smd11
- **修改内容**: 将 `DEFAULT_SERIAL_PORT` 从 `/dev/ttyIN` 改为 `/dev/smd11`
- **原因**: 设备默认使用 SMD 通道 `/dev/smd11` 进行 AT 通信，无需 socat PTY 桥接
- **涉及文件**:
  - `src/main.rs`

## 2026-05-29 00:19 — 修复串口打开失败问题
- **问题**: 代码使用 `tokio::fs::OpenOptions` 直接打开串口设备文件，未配置串口参数（波特率、数据位等），导致 `Not a typewriter` 错误
- **修改内容**:
  - 新增 `open_serial()` 函数，使用 `tokio_serial` 正确配置 115200 8N1
  - `fetch_static_info()` 替换 `OpenOptions::open()` 为 `open_serial()`
  - `hardware_polling_actor()` 替换 `OpenOptions::open()` 为 `open_serial()`
  - `send_at_command_async()` 参数类型 `&mut File` → `&mut SerialStream`
- **涉及文件**:
  - `src/main.rs`

## 2026-05-29 01:50 — 增强全流程日志：增加诊断信息便于定位问题
- **修改内容**:
  - `drain_serial_buffer()`: 增加残留数据首段 ASCII 内容输出，可判断残留数据来源（开机日志/前次响应）
  - `open_serial()`: 增加打开耗时统计，失败时显示完整路径和耗时
  - `send_at_command_async()`: 增加 AT 命令执行耗时统计，正确响应输出完整内容，ERROR 响应单独标记 ⚠️
  - `fetch_static_info()`: 增加分步进度日志 [1/5]～[5/5]，模拟模式写入提示，最终结果包含全部 5 个字段
  - `hardware_polling_actor()` 轮询循环:
    - 合并命令增加发送/解析阶段耗时统计
    - 降级发送时每个命令独立标记失败原因
    - 增加 SIM 状态、静态信息、解析阶段耗时输出
    - WS 广播前输出接收者数量和 JSON 前 200 字节预览
    - 序列化失败时标记 ❌
  - `handle_ws()`: 增加各 WS 动作的日志（manual_at 显示命令内容、set_interval 显示秒数、set_view_state 显示切换详情、get_static_info 显示发送的静态信息）、未知 action 标记 ⚠️、无法解析的 WS 消息标记 ⚠️
- **涉及文件**:
  - `src/main.rs`
  - `CLINE_HISTORY.md`
## 2026-05-29 01:12 — ADB 实际调试：发现 AT 通道为 socat PTY 桥接 /dev/ttyIN
- **问题**: SMD 通道（smd7/smd8/smd11/smd21/smd22）和 ttyMSM0/ttyGS0 全部无 AT 响应，程序卡在读取超时
- **调试过程**:
  - `adb shell "ls -la /dev/smd* /dev/tty*"` → 列出所有串口设备
  - `adb shell "ps | grep -iE 'ril|modem|qmux|qmi|at|radio'"` → 发现有 `socat-at-bridge` 进程：
    `socat-armel-static pty,link=/dev/ttyIN,raw,echo=0 pty,link=/dev/ttyOUT,raw,echo=1`
  - `adb shell "echo -ne 'AT\r\n' > /dev/ttyIN && sleep 0.3 && timeout 2 cat /dev/ttyOUT"` → 确认 /dev/ttyIN 返回 AT 响应（FW: RG520NEUDCR01A02M4G_TP）
- **修复内容**:
  - 默认串口从 `/dev/ttyMSM0` 改为 `/dev/ttyIN`（socat PTY 桥接 AT 通道）
  - 新增 `skip_tty_mode()` 函数：socat PTY 桥接设备（ttyIN/ttyOUT）不能用 `tokio_serial` 打开（tcsetattr 会破坏 socat 的 raw 配置），必须跳过 TTY 模式直接用 Raw 文件模式
  - `open_serial()` 增加 TTY 跳过逻辑，先判断是否 socat 设备，再决定用哪种模式
- **涉及文件**:
  - `src/main.rs`
  - `CLINE_HISTORY.md`

## 2026-05-29 00:58 — SMD 通道通信修复 + Skill 文档更新
- **问题**: `/dev/smd11` (Qualcomm SMD) Raw 模式下 AT 命令返回空或无响应
- **修复内容**:
  - 新增 `drain_serial_buffer()` 函数，启动时清空串口残留数据
  - `fetch_static_info()` 中对 Raw 模式增加 ATE0 初始化握手（关闭回显）
  - `hardware_polling_actor()` 中增加 SMD 初始化握手（drain + ATE0）
  - `send_at_command_async()` 中 0 字节读取不再直接 break，改为连续 3 次后才退出（SMD 通道 0 字节不代表 EOF）
- **Skill 文档更新**:
  - `.clinerules` 重写了 Run Commands 节，拆分为 Cross-compilation (armhf)、ADB Deployment、Run Commands 三个独立章节
  - 新增 armv7 musl 交叉编译文档（target setup、build 命令、产物路径）
  - 新增 ADB 部署完整指南（前置条件、常用命令、一键构建部署、只推送、手动运行、日志查看、串口排查）
- **涉及文件**:
  - `src/main.rs`
  - `.clinerules`
  - `CLINE_HISTORY.md`
  - `Cargo.toml`

## 2026-05-29 00:28 — 创建 .clinerules 和 .clineignore
- **描述**: 为项目创建 Cline 遵守规则文件和忽略文件清单
- **涉及文件**:
  - `.clinerules`
  - `.clineignore`

## 2026-05-29 00:31 — 添加 AI 修改历史记录规则
- **描述**: 在 `.clinerules` 中添加 `AI Change History` 节，要求每次 AI 修改代码必须追加记录到 `CLINE_HISTORY.md`
- **涉及文件**:
  - `.clinerules`
  - `CLINE_HISTORY.md`

## 2026-05-29 00:36 — 修复串口卡死（日志停在"串口设备: /dev/smd11"无下文）
- **问题**: 代码使用 `tokio::fs::File`（`OpenOptions`）打开串口设备 `/dev/smd11`，但由于未配置波特率等串口参数，`read()` 调用永久阻塞，导致程序卡在 `fetch_static_info`
- **根因**: `tokio::fs::File` 是普通文件 I/O，操作串口设备时不设置波特率、数据位、停止位等参数，系统内核无法正确读取串口数据，`read` 调用无限等待
- **修改内容**:
  - 新增 `open_serial()` 函数，使用 `tokio_serial::SerialStream` 并配置 115200 波特率
  - `fetch_static_info()` 改用 `open_serial()` 临时打开串口，完成后主动 `drop` 关闭（供 `hardware_polling_actor` 后续独立重开）
  - `hardware_polling_actor()` 改用 `open_serial()` 打开独立串口连接
  - `send_at_command_async()` 参数类型从 `&mut File` 改为 `&mut SerialStream`
  - 移除不再使用的 `tokio::fs::{File, OpenOptions}` 和 `tokio::io` 中不再需要的导入
- **涉及文件**:
  - `src/main.rs`

## 2026-05-29 01:52 — 增强全流程诊断日志（含串口 read 超时优化）
- **修改内容**:
  - `drain_serial_buffer()`: 增加残留数据首段 ASCII 内容输出，可判断残留数据来源（开机日志/前次响应）
  - `open_serial()`: 增加打开耗时统计，失败时显示完整路径和耗时
  - `send_at_command_async()`: 增加 AT 命令执行耗时统计和完整响应输出；ERROR 返回标记 ⚠️；串口 read 超时从 10s **降为 500ms**；新增 **5 秒总超时截止** 防止单个 AT 永久卡死
  - `fetch_static_info()`: 增加分步进度 [1/5]～[5/5]；模拟模式写入提示；最终结果包含全部 5 个字段
  - `hardware_polling_actor()` 轮询循环: 合并命令增加发送/解析阶段耗时统计；降级发送时每个命令独立标记失败原因；SIM 状态、静态信息、解析耗时输出；WS 广播前输出接收者数量和 JSON 前 200 字节预览；序列化失败标记 ❌
  - `handle_ws()`: 各 WS 动作请求日志（manual_at 命令内容、set_interval 秒数、set_view_state 切换详情、get_static_info 发送内容）；未知 action 标记 ⚠️；无法解析的 WS 消息标记 ⚠️
  - 移除不再使用的 `IO_TIMEOUT` 常量（由 500ms read 超时替代）
- **实机测试结果**: 程序在 `/dev/smd11` 上 `drain_serial_buffer` 完成后卡死，无 `ATE0` 响应输出、无超时错误输出。`send_at_command_async` 的 5s 总超时也未触发，表明 `write_all()` 写入操作本身即永久阻塞，而非 read 阶段卡住。SMD 通道 `/dev/smd11` 写入卡死，模块无应答
- **涉及文件**:
  - `src/main.rs`
  - `CLINE_HISTORY.md`

## 2026-06-03 22:49 — 修复 fetch_static_info 启动卡死：增加全局超时保护
- **对比 commit**: 94bfaced5d68dcf99cfa00e44c9e657da1f38e66
- **问题**: `fetch_static_info()` 在 `main()` 中同步 await，如果串口设备无响应（SMD 通道 write 阻塞），会导致整个启动流程永久卡死，`hardware_polling_actor` 永远不会被 spawn，HTTP 服务器永远不会启动
- **根因**: `fetch_static_info().await` 没有全局超时保护。每个 AT 命令虽有 5s 超时，但如果 `send_at_command_async` 内部的同步 `write_all()` 在 OS 层面永久阻塞（如 SMD 驱动无响应），tokio 的超时也无法取消正在执行的同步阻塞 syscall
- **修改内容**:
  - `main()` 中用 `tokio::time::timeout(Duration::from_secs(30), fetch_static_info(...))` 包裹，30s 后取消 Future，写入默认静态信息并继续启动 HTTP 服务器和 hardware_polling_actor
  - 移除 `fetch_static_info()` 中 DEBUG `eprintln!` 日志（drain_serial_buffer/ATE0/AT/AT+CGMR 原始响应）
  - 移除不再使用的 `IO_TIMEOUT` 常量
- **涉及文件**:
  - `src/main.rs`
  - `CLINE_HISTORY.md`

## 2026-06-03 23:00 — 修复 O_NONBLOCK 模式 write 返回 EBUSY 导致所有 AT 命令失败
- **问题**: `open_serial()` 以 `O_NONBLOCK` 打开 `/dev/smd11` 后，`write_all()` 使用 `std::fs::File::write_all()` 同步写入，立即返回 `Resource busy (EBUSY, os error 16)`，导致所有 AT 命令（ATE0, AT, AT+CGMR 等）全部失败
- **根因**: `O_NONBLOCK` 设备的 write 操作在内核层可能因流控/资源竞争返回 EBUSY，原有的 `write_all()` 将其视为硬错误直接返回失败，没有重试机制
- **修改内容**:
  - `SerialHandle::write_all()` 从同步 `f.write_all()` 改为异步循环：逐块 `f.write()` 写入，遇到 `WouldBlock` 或 `EBUSY` 时 `sleep(10ms)` 重试，5s 超时
  - 相应在 `send_at_command_async()` 中 `write_all()` 调用后面补上 `.await`
- **涉及文件**:
  - `src/main.rs`
  - `CLINE_HISTORY.md`

## 2026-06-03 23:22 — 串口改为每次 AT 命令打开/关闭
- **问题**: 串口保持持久打开导致 socat PTY 桥接设备状态异常、资源占用
- **修改内容**:
  - 原 `send_at_command_async(serial, cmd)` 重命名为 `send_at_command_inner(serial, cmd)`（纯底层发送逻辑，仍接受 `&mut SerialHandle`）
  - 新增 `send_at_command_async(path, cmd)` 包装函数：每次调用 `open_serial()` → `drain_serial_buffer()` → `send_at_command_inner()` → drop（自动关闭串口）
  - `fetch_static_info()`: 移除持久串口句柄 `serial`，所有 `send_at_command_async(&mut serial, ...)` 改为 `send_at_command_async(serial_path, ...)`，模拟检测改为单次 `open_serial().is_none()` 判断
  - `hardware_polling_actor()`: 移除 `Option<SerialHandle>` 持久句柄，改为 `bool serial_available`，所有 AT 调用通过 `send_at_command_async(&serial_path, ...)` 每次独立打开/关闭
- **涉及文件**:
  - `src/main.rs`
  - `CLINE_HISTORY.md`
