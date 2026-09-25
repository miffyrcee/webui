//! 硬件独占 Actor 调度层：静态信息读取 + 定时轮询 + 用户 AT 指令严格串行执行（0 锁）。

pub mod action;
pub mod telemetry;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use self::action::{AtAction, AtRequest, DiagnosticType};
use self::telemetry::{GlobalTelemetry, serialize_global};
use crate::at::{
    builder::sanitize_at_param, decode_cmgl_body, normalize_at_command, parse_qeng_neighbour,
};
use crate::backend::{
    HardwareBackend, ImeiParseResult, at_exec_failed, at_response_preview, parse_egmr_response,
};
use crate::logger::push_log;
use crate::sys::{CpuSnapshot, get_cpu_usage, get_memory_usage, get_soc_temperature, get_uptime_mins};
use crate::web::state::AppState;

const MAX_CONSECUTIVE_FAILURES: u32 = 3;
const IDLE_INTERVAL_SECS: u64 = 15;

/// 处理单个 channel 消息
pub async fn handle_channel_req(
    req: Option<AtRequest>,
    backend: &Arc<dyn HardwareBackend>,
    interval_secs: &mut u64,
    interval: &mut tokio::time::Interval,
    current_interval_secs: &mut u64,
) -> bool {
    match req {
        Some(r) => {
            handle_at_request(r, backend, interval_secs, interval, current_interval_secs).await;
            true
        }
        None => false,
    }
}

/// 处理单个 AT 请求
pub async fn handle_at_request(
    req: AtRequest,
    backend: &Arc<dyn HardwareBackend>,
    interval_secs: &mut u64,
    interval: &mut tokio::time::Interval,
    current_interval_secs: &mut u64,
) {
    match req.action {
        AtAction::ManualAt(cmd) => {
            let cmd = normalize_at_command(&cmd);
            push_log("INFO", "Actor", &format!("顺序执行手动 AT: {}", cmd));
            let response = backend.exec_raw_at(&cmd).await;
            let _ = req
                .resp_tx
                .send(serde_json::json!({ "type": "at_res", "data": response }));
        }
        AtAction::SetInterval(secs) => {
            let new_secs = secs.max(3);
            *interval_secs = new_secs;
            *current_interval_secs = new_secs;
            *interval = tokio::time::interval(Duration::from_secs(*interval_secs));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await;
            push_log(
                "INFO",
                "Settings",
                &format!("轮询间隔已调整为 {} 秒", new_secs),
            );
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "settings_log",
                "data": { "msg": format!("轮询间隔已调整为 {} 秒", new_secs) }
            }));
        }
        AtAction::GetSmsList => {
            push_log("INFO", "Actor", "读取短信列表...");
            let resp = backend.read_sms_list().await;
            let decoded = decode_cmgl_body(&resp);
            let _ = req
                .resp_tx
                .send(serde_json::json!({ "type": "sms_list", "data": decoded }));
        }
        AtAction::SetApn(apn, user, pass, auth_type) => {
            push_log("INFO", "Actor", &format!("配置 APN: {}", apn));
            let res = backend.configure_apn(&apn, &user, &pass, auth_type).await;
            let (success, msg) = match res {
                Ok(m) => (true, m),
                Err(e) => (false, e),
            };
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "network_status",
                "data": {
                    "status": if success { "APN settings applied" } else { "APN settings failed" },
                    "success": success,
                    "msg": msg,
                    "apn": apn
                }
            }));
        }
        AtAction::SetNetworkMode(mode) => {
            push_log("INFO", "Actor", &format!("切换网络模式为: {}", mode));
            let res = backend.set_network_mode_pref(&mode).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "network_status",
                "data": { "success": res.is_ok(), "msg": res.unwrap_or_else(|e| e) }
            }));
        }
        AtAction::NetConnect(connect) => {
            push_log(
                "INFO",
                "Actor",
                &format!("拨号控制: {}", if connect { "连接" } else { "断开" }),
            );
            let res = backend.set_data_session(connect).await;
            let (success, msg) = match res {
                Ok(m) => (true, m),
                Err(e) => (false, e),
            };
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "network_status",
                "data": {
                    "status": format!("{} command {}", if connect { "Connect" } else { "Disconnect" }, if success { "sent" } else { "failed" }),
                    "success": success,
                    "msg": msg
                }
            }));
        }
        AtAction::NetworkScan => {
            push_log("INFO", "Actor", "开始扫描可用网络...");
            let networks = backend.scan_available_networks().await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "scan_result",
                "data": { "status": "Scan complete.", "networks": networks }
            }));
        }
        AtAction::SendSms(recipient, message) => {
            push_log("INFO", "Actor", &format!("发送短信至: {}", recipient));
            let success = backend.send_sms_msg(&recipient, &message).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "sms_sent",
                "data": { "status": if success { "SMS sent successfully" } else { "SMS failed" }, "recipient": recipient }
            }));
        }
        AtAction::GetDeviceInfo => {
            push_log("INFO", "Actor", "获取设备详细信息...");
            let info = backend.read_device_info().await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "device_info",
                "data": {
                    "manufacturer": info.manufacturer,
                    "model": info.model,
                    "firmware_version": info.firmware,
                    "imei": info.imei,
                    "serial": info.serial,
                    "hw_version": info.hw_version,
                    "module_type": info.module_type,
                    "sim_status": info.sim_status,
                    "imsi": info.imsi,
                    "iccid": info.iccid,
                    "phone": info.phone,
                    "net_status": info.net_status,
                    "signal": info.signal_quality,
                    "temperature": info.temperature,
                    "bands": info.bands,
                    "max_rate": "--",
                    "volte": "--",
                    "gnss": "--"
                }
            }));
        }
        AtAction::Reboot => {
            push_log("WARN", "System", "重启模组指令已下发");
            backend.send_reboot().await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "settings_log",
                "data": { "msg": "Reboot command sent to module." }
            }));
        }
        AtAction::FactoryReset => {
            push_log("WARN", "System", "模组恢复出厂设置指令已下发");
            backend.send_factory_reset().await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "settings_log",
                "data": { "msg": "Factory reset command sent to module." }
            }));
        }
        AtAction::FlightMode(on) => {
            push_log(
                "INFO",
                "System",
                &format!("飞行模式状态改变: {}", if on { "开启" } else { "关闭" }),
            );
            backend.set_airplane_mode(on).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "settings_log",
                "data": { "msg": format!("Flight mode turned {}", if on { "ON" } else { "OFF" }) }
            }));
        }
        AtAction::SetBandLock { is_nr5g, bands } => {
            push_log(
                "INFO",
                "Actor",
                &format!("设置频段锁定: is_nr5g={}, bands={}", is_nr5g, bands),
            );
            let res = backend.set_band_lock(is_nr5g, &bands).await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "band_lock_res",
                "data": { "success": res.is_ok(), "msg": res.unwrap_or_else(|e| e) }
            }));
        }
        AtAction::SetCellLock {
            tech,
            pci,
            earfcn,
            band,
            enable,
        } => {
            push_log(
                "INFO",
                "Actor",
                &format!(
                    "设置小区锁定: tech={}, pci={}, earfcn={}, band={:?}, enable={}",
                    tech, pci, earfcn, band, enable
                ),
            );
            let res = backend
                .set_cell_lock(&tech, pci, earfcn, band, enable)
                .await;
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "cell_lock_res",
                "data": { "success": res.is_ok(), "msg": res.unwrap_or_else(|e| e) }
            }));
        }
        AtAction::GetDiagnostics(diag) => {
            push_log("INFO", "Actor", &format!("获取诊断信息: {}", diag));
            let res = backend.get_diagnostics(&diag).await;
            match diag {
                DiagnosticType::Neighbour => {
                    let raw = res.clone().unwrap_or_default();
                    let cells = parse_qeng_neighbour(&raw);
                    let _ = req.resp_tx.send(serde_json::json!({
                        "type": "diagnostics_res",
                        "data": {
                            "success": res.is_ok(),
                            "msg": res.unwrap_or_else(|e| e),
                            "data": {
                                "raw": raw,
                                "cells": cells,
                            }
                        }
                    }));
                }
                _ => {
                    let data = res
                        .as_ref()
                        .ok()
                        .map(|r| serde_json::Value::String(r.clone()))
                        .unwrap_or(serde_json::Value::Null);
                    let _ = req.resp_tx.send(serde_json::json!({
                        "type": "diagnostics_res",
                        "data": { "success": res.is_ok(), "msg": res.unwrap_or_else(|e| e), "data": data }
                    }));
                }
            }
        }
        AtAction::SetUsbNetMode(mode) => {
            push_log("INFO", "Actor", &format!("设置 USB 网络模式为: {}", mode));
            let res = backend.set_usb_net_mode(mode).await;

            // 设置成功后主动回读当前配置，随响应一并推送，替代前端固定延时轮询
            let config = if res.is_ok() {
                match backend.get_usb_config().await {
                    Ok(c) => Some(c),
                    Err(e) => {
                        push_log(
                            "WARN",
                            "Actor",
                            &format!("设置 USB 模式后回读配置失败: {}", e),
                        );
                        None
                    }
                }
            } else {
                None
            };

            let _ = req.resp_tx.send(serde_json::json!({
                "type": "usb_net_res",
                "data": {
                    "success": res.is_ok(),
                    "msg": res.unwrap_or_else(|e| e),
                    "note": "注意：USB 模式修改后通常需要重启模组方可生效！",
                    "config": config
                }
            }));
        }
        AtAction::GetUsbConfig => {
            push_log("INFO", "Actor", "获取 USB 配置信息...");
            match backend.get_usb_config().await {
                Ok(data) => {
                    let _ = req.resp_tx.send(serde_json::json!({
                        "type": "usb_config_info",
                        "data": { "success": true, "config": data }
                    }));
                }
                Err(e) => {
                    let _ = req.resp_tx.send(serde_json::json!({
                        "type": "usb_config_info",
                        "data": { "success": false, "error": e }
                    }));
                }
            }
        }
        AtAction::SetSimSlot(slot) => {
            push_log("INFO", "Actor", &format!("切换 SIM 卡槽为: {}", slot));
            let res = backend.set_sim_slot(slot).await;
            let (success, msg) = match res {
                Ok(_) => (true, "OK".to_string()),
                Err(e) => (false, e),
            };
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "sim_slot_res",
                "data": {
                    "success": success,
                    "slot": slot,
                    "msg": msg,
                    "note": "切换卡槽后需重启模组方可生效"
                }
            }));
        }
        AtAction::GetMbnList => {
            push_log("INFO", "Actor", "查询 MBN 列表...");
            match backend.get_mbn_list().await {
                Ok(list) => {
                    let _ = req.resp_tx.send(serde_json::json!({
                        "type": "mbn_list_res",
                        "data": { "success": true, "list": list }
                    }));
                }
                Err(e) => {
                    let _ = req.resp_tx.send(serde_json::json!({
                        "type": "mbn_list_res",
                        "data": { "success": false, "msg": e }
                    }));
                }
            }
        }
        AtAction::SetMbn(name) => {
            push_log("INFO", "Actor", &format!("选择 MBN: {}", name));
            let res = backend.set_mbn(&name).await;
            let (success, msg) = match res {
                Ok(_) => (true, "OK".to_string()),
                Err(e) => (false, e),
            };
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "mbn_set_res",
                "data": {
                    "success": success,
                    "msg": msg,
                    "note": "选择 MBN 后需重启模组方可生效"
                }
            }));
        }
        AtAction::SetMbnAutoSel(on) => {
            push_log(
                "INFO",
                "Actor",
                &format!("MBN AutoSel 切换: {}", if on { "启用" } else { "禁用" }),
            );
            let cmd = format!("AT+QMBNCFG=\"AutoSel\",{}", if on { 1 } else { 0 });
            let res = backend.exec_raw_at(&cmd).await;
            let ok = !at_exec_failed(&res);
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "mbn_set_res",
                "data": {
                    "success": ok,
                    "msg": if ok {
                        (if on { "MBN AutoSel 已启用" } else { "MBN AutoSel 已禁用（锁定当前 MBN）" }).to_string()
                    } else { at_response_preview(&res).to_string() }
                }
            }));
        }
        AtAction::DeactivateMbn => {
            push_log("INFO", "Actor", "停用当前 MBN");
            let res = backend.exec_raw_at("AT+QMBNCFG=\"Deactivate\"").await;
            let ok = !at_exec_failed(&res);
            let _ = req.resp_tx.send(serde_json::json!({
                "type": "mbn_set_res",
                "data": {
                    "success": ok,
                    "msg": if ok { "当前 MBN 已停用".to_string() } else { at_response_preview(&res).to_string() },
                    "note": if ok { "需重启模组方可生效" } else { "" }
                }
            }));
        }
        AtAction::ReadImei => {
            push_log("INFO", "Actor", "读取 IMEI...");
            let raw = backend.exec_raw_at("AT+EGMR=0,7").await;
            let parsed = parse_egmr_response(&raw).unwrap_or(ImeiParseResult {
                kind: "read".to_string(),
                success: false,
                imei: None,
            });
            let _ = req
                .resp_tx
                .send(serde_json::json!({ "type": "imei_res", "data": parsed }));
        }
        AtAction::WriteImei(imei) => {
            let imei = sanitize_at_param(&imei);
            push_log("INFO", "Actor", &format!("写入 IMEI: {}", imei));
            let raw = backend
                .exec_raw_at(&format!("AT+EGMR=1,7,\"{}\"", imei))
                .await;
            let parsed = parse_egmr_response(&raw).unwrap_or_else(|| {
                // 无回显时模组写入成功仅返回 OK（不含 AT+EGMR 回显）
                let ok = !at_exec_failed(&raw) && raw.contains("OK");
                ImeiParseResult {
                    kind: "write".to_string(),
                    success: ok,
                    imei: None,
                }
            });
            let _ = req
                .resp_tx
                .send(serde_json::json!({ "type": "imei_res", "data": parsed }));
        }
        AtAction::SetEthConfig { driver, pcie_rc } => {
            push_log(
                "INFO",
                "Actor",
                &format!("开始原子配置网口: driver={}, pcie_rc={}", driver, pcie_rc),
            );

            // 步骤 1: 加载网卡 PHY 驱动
            let r1 = backend
                .exec_raw_at(&format!("AT+QETH=\"eth_driver\",\"{}\",1", driver))
                .await;
            if at_exec_failed(&r1) {
                let _ = req.resp_tx.send(serde_json::json!({
                    "type": "eth_res",
                    "data": { "success": false, "msg": format!("加载网卡驱动失败: {}", r1.trim()) }
                }));
                return;
            }

            // 步骤 2: 配置 PCIe 总线模式
            let r2 = backend
                .exec_raw_at(&format!(
                    "AT+QCFG=\"pcie/mode\",{}",
                    if pcie_rc { 1 } else { 0 }
                ))
                .await;
            if at_exec_failed(&r2) {
                let _ = req.resp_tx.send(serde_json::json!({
                    "type": "eth_res",
                    "data": { "success": false, "msg": format!("设置 PCIe 模式失败: {}", r2.trim()) }
                }));
                return;
            }

            // 步骤 3: PCIe RC 主机模式时启用数据通道
            if pcie_rc {
                let r3 = backend.exec_raw_at("AT+QCFG=\"data_interface\",1,0").await;
                if at_exec_failed(&r3) {
                    let _ = req.resp_tx.send(serde_json::json!({
                        "type": "eth_res",
                        "data": { "success": false, "msg": format!("设置数据通道失败: {}", r3.trim()) }
                    }));
                    return;
                }
            }

            let _ = req.resp_tx.send(serde_json::json!({
                "type": "eth_res",
                "data": { "success": true, "msg": "网口配置全部下发成功，请热重启 (CFUN=1,1) 或断电重启使网卡生效" }
            }));
        }
        AtAction::SetIpptConfig { mode } => {
            push_log(
                "INFO",
                "Actor",
                &format!("开始原子配置 IP 直通: mode={}", mode),
            );

            // 步骤 1: 开启网口自动拨号
            let r1 = backend.exec_raw_at("AT+QMAPWAC=1").await;
            if at_exec_failed(&r1) {
                let _ = req.resp_tx.send(serde_json::json!({
                    "type": "ippt_res",
                    "data": { "success": false, "msg": format!("开启自动拨号失败: {}", r1.trim()) }
                }));
                return;
            }

            // 步骤 2: 下发 LANIP 地址池
            let lanip = if mode == "dmz" {
                "AT+QMAP=\"LANIP\",192.168.225.2,192.168.225.2,192.168.225.1,1"
            } else {
                "AT+QMAP=\"LANIP\",192.168.225.20,192.168.225.100,192.168.225.1,1"
            };
            let r2 = backend.exec_raw_at(lanip).await;
            if at_exec_failed(&r2) {
                let _ = req.resp_tx.send(serde_json::json!({
                    "type": "ippt_res",
                    "data": { "success": false, "msg": format!("设置 LANIP 失败: {}", r2.trim()) }
                }));
                return;
            }

            // 步骤 3: 下发 DMZ 映射 / 关闭 DMZ
            let dmz = if mode == "dmz" {
                "AT+QMAP=\"DMZ\",1,4,192.168.225.2"
            } else {
                "AT+QMAP=\"DMZ\",0"
            };
            let r3 = backend.exec_raw_at(dmz).await;
            if at_exec_failed(&r3) {
                let _ = req.resp_tx.send(serde_json::json!({
                    "type": "ippt_res",
                    "data": { "success": false, "msg": format!("设置 DMZ 失败: {}", r3.trim()) }
                }));
                return;
            }

            let _ = req.resp_tx.send(serde_json::json!({
                "type": "ippt_res",
                "data": { "success": true, "msg": "直通配置全部下发成功，请热重启 (CFUN=1,1) 或断电重启使配置生效" }
            }));
        }
    }
}

/// 统一硬件任务：静态信息读取 + 定时轮询 + 用户 AT 指令。严格串行 Actor + 0 锁
pub async fn hardware_task(
    mut command_rx: mpsc::Receiver<AtRequest>,
    backend: Arc<dyn HardwareBackend>,
    telemetry_tx: watch::Sender<Arc<GlobalTelemetry>>,
    state: Arc<AppState>,
) {
    // Phase 1: 启动时读取静态信息（FW 版本、SIM 槽、运营商、APN）
    // 放在 hardware_task 开头以确保串口独占，无需任何锁
    push_log("INFO", "Actor", "开始获取静态信息...");
    let (firmware_version, active_sim, network_provider, apn) = backend.read_static_info().await;
    {
        let initial_global = Arc::new(GlobalTelemetry {
            firmware_version: Some(firmware_version),
            active_sim: Some(active_sim),
            network_provider: Some(network_provider),
            apn: Some(apn),
            ..Default::default()
        });
        let _ = telemetry_tx.send(initial_global.clone());
        let json_str = serialize_global(&initial_global, "full");
        let _ = state.tx.send(Arc::new(json_str));
    }
    push_log("INFO", "Actor", "静态信息获取完成，进入轮询循环");

    // Phase 2: 轮询循环
    let mut prev_cpu: Option<CpuSnapshot> = None;
    let mut interval_secs = 5u64;
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await;
    let mut consecutive_failures = 0u32;
    let mut current_interval_secs = interval_secs;

    loop {
        tokio::select! {
            biased;

            req = command_rx.recv() => {
                if !handle_channel_req(req, &backend, &mut interval_secs, &mut interval, &mut current_interval_secs).await {
                    break;
                }
            }
            _ = interval.tick() => {
                let active = state.active_views.load(Ordering::Acquire) > 0;

                if active {
                    let start_poll = std::time::Instant::now();
                    let mut telemetry = backend.poll_telemetry().await;

                    let telemetry_dead = telemetry.sim_status.is_none()
                        && telemetry.network_mode.is_none()
                        && telemetry.signal_percentage.is_none()
                        && telemetry.ipv4.is_none();
                    consecutive_failures = if telemetry_dead { consecutive_failures + 1 } else { 0 };

                    if telemetry.temperature.is_none() {
                        telemetry.temperature = get_soc_temperature();
                    }

                    telemetry.internet_connection = Some(if telemetry.ipv4.is_some() {
                        "Connected".to_string()
                    } else {
                        "Disconnected".to_string()
                    });

                    let uptime_mins = get_uptime_mins();
                    let cpu_usage = get_cpu_usage(&mut prev_cpu);
                    let memory_usage = get_memory_usage();
                    let updated_str = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();

                    // 构建新全局状态（零锁：从 watch 读取当前值，构建新 Arc 后发送）
                    let new_global = if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        push_log("ERROR", "Poll", &format!("模组连续 {} 次无响应，标记为离线", MAX_CONSECUTIVE_FAILURES));
                        let current = state.telemetry_rx.borrow();
                        let fw = current.firmware_version.clone();
                        let sim = current.active_sim.clone();
                        let prov = current.network_provider.clone();
                        let apn_val = current.apn.clone();
                        drop(current);
                        let mut g = GlobalTelemetry::default();
                        g.firmware_version = fw;
                        g.active_sim = sim;
                        g.network_provider = prov;
                        g.apn = apn_val;
                        g.internet_connection = Some("Disconnected".to_string());
                        g.uptime = Some(format!("{} minutes", uptime_mins));
                        g.cpu_usage = cpu_usage;
                        g.memory_usage = memory_usage;
                        g.updated = Some(updated_str);
                        g
                    } else {
                        let current = state.telemetry_rx.borrow();
                        let mut g = GlobalTelemetry::from_telemetry_and_global(
                            &telemetry, &current,
                            Some(format!("{} minutes", uptime_mins)),
                            Some(updated_str),
                        );
                        drop(current);
                        if g.cpu_usage.is_none() {
                            g.cpu_usage = cpu_usage;
                        }
                        if g.memory_usage.is_none() {
                            g.memory_usage = memory_usage;
                        }
                        g
                    };

                    let arc_global = Arc::new(new_global);
                    let _ = telemetry_tx.send(arc_global.clone());

                    // 预序列化为 JSON 字符串并广播（零重复序列化损耗）
                    let json_str = serialize_global(&arc_global, "delta");
                    let _ = state.tx.send(Arc::new(json_str));

                    push_log("INFO", "Poll", &format!("轮询完成 (耗时 {}ms)", start_poll.elapsed().as_millis()));

                } else {
                    push_log("INFO", "Poll", "硬件轮询处于空闲模式 (无活跃视图)");
                }

                let next_secs = if active { interval_secs } else { IDLE_INTERVAL_SECS };
                if next_secs != current_interval_secs {
                    current_interval_secs = next_secs;
                    interval = tokio::time::interval(Duration::from_secs(next_secs));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    interval.tick().await;
                }
            }
        }
    }
}
