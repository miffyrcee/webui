//! WebSocket 连接管理与指令分发器。

use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::{mpsc, oneshot};

use crate::actor::action::{AtAction, AtRequest, DiagnosticType};
use crate::actor::telemetry::serialize_global;
use crate::auth::is_authenticated;
use crate::logger::{get_logs_snapshot, push_log};
use crate::web::state::{ActiveViewGuard, AppState, safe_dec_active_views};

#[derive(Serialize, Deserialize, Debug)]
pub struct WsCommand {
    pub action: String,
    pub payload: Option<serde_json::Value>,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state.jwt_secret) {
        return (StatusCode::UNAUTHORIZED, "Unauthorized WebSocket Request").into_response();
    }

    // 跨站 WebSocket 劫持 (CSWSH) 防护：校验 Origin 必须与 Host 一致
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());

    if let (Some(host_val), Some(origin_val)) = (host, origin) {
        // 使用 :// 正确切割协议头，避免 trim_start_matches 链式调用的缺陷
        let origin_host = origin_val.split("://").nth(1).unwrap_or(origin_val);
        // 仅对默认 HTTP/HTTPS 端口剥离，显式其他端口保留参与比对
        let origin_compare = origin_host
            .strip_suffix(":80")
            .or_else(|| origin_host.strip_suffix(":443"))
            .unwrap_or(origin_host);
        let host_compare = host_val
            .strip_suffix(":80")
            .or_else(|| host_val.strip_suffix(":443"))
            .unwrap_or(host_val);
        if origin_compare != host_compare {
            push_log(
                "WARN",
                "WS",
                &format!(
                    "拦截跨站 WebSocket 劫持: Origin {} != Host {}",
                    origin_val, host_val
                ),
            );
            return (StatusCode::FORBIDDEN, "Cross-Site WebSocket Request Denied").into_response();
        }
    }

    ws.on_upgrade(|socket| handle_ws(socket, state))
        .into_response()
}

pub async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    // 收到 Arc<String> 预序列化广播，零反序列化/序列化开销
    let mut broadcast_rx = state.tx.subscribe();
    let online_count = state.tx.receiver_count();
    push_log(
        "INFO",
        "WS",
        &format!("新的 WebSocket 客户端已连接 (当前在线: {})", online_count),
    );

    // RAII Guard：离开作用域自动递减 active_views（Panic/Cancel 安全）
    let (_guard, is_view_active) = ActiveViewGuard::new(state.clone());
    let mut current_is_active = true;

    let (local_tx, mut local_rx) = mpsc::channel::<String>(10);
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // 发送全量快照作为第一条消息（零锁读取 watch，borrow 不跨越 .await）
    let full_json = {
        let guard = state.telemetry_rx.borrow();
        serialize_global(&guard, "full")
    };
    if ws_sender
        .send(Message::Text(full_json.into()))
        .await
        .is_err()
    {
        return; // _guard.drop() 自动递减
    }

    tokio::select! {
        _ = async {
            loop {
                tokio::select! {
                    result = broadcast_rx.recv() => {
                        match result {
                            Ok(pre_serialized) => {
                                if ws_sender.send(Message::Text((*pre_serialized).clone().into())).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => continue, // 通道满跳过旧消息或已关闭
                        }
                    }
                    Some(reply) = local_rx.recv() => {
                        if ws_sender.send(Message::Text(reply.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            push_log("WARN", "WS", "WebSocket 发送通道中断，准备断开连接");
        } => {},

        _ = async {
            let state_inner = state.clone();
            while let Some(Ok(Message::Text(text))) = ws_receiver.next().await {
                if let Ok(cmd) = serde_json::from_str::<WsCommand>(&text) {
                    let (resp_tx, resp_rx) = oneshot::channel();

                    let action = match cmd.action.as_str() {
                        "manual_at" => {
                            let payload = cmd.payload.and_then(|p| p.as_str().map(String::from)).unwrap_or_default();
                            AtAction::ManualAt(payload)
                        }
                        "read_imei" => AtAction::ReadImei,
                        "write_imei" => {
                            let imei = cmd.payload.and_then(|p| p.as_str().map(String::from)).unwrap_or_default();
                            AtAction::WriteImei(imei)
                        }
                        "set_interval" => {
                            let secs = cmd.payload.as_ref().and_then(|p| {
                                p.as_u64().or_else(|| p.as_str().and_then(|s| s.parse::<u64>().ok()))
                            }).unwrap_or(3);
                            AtAction::SetInterval(secs)
                        }
                        "set_view_state" => {
                            let is_active = cmd.payload.as_ref().and_then(|p| p.as_str()) == Some("active");
                            if is_active != current_is_active {
                                if is_active {
                                    state_inner.active_views.fetch_add(1, Ordering::SeqCst);
                                } else {
                                    safe_dec_active_views(&state_inner.active_views);
                                }
                                current_is_active = is_active;
                                // 同步 RAII Guard 的活跃标志，保证 Drop 时正确递减
                                is_view_active.store(is_active, Ordering::SeqCst);
                            }
                            continue;
                        }
                        "get_static_info" => {
                            let info_json = {
                                let guard = state_inner.telemetry_rx.borrow();
                                serde_json::json!({
                                    "type": "static_info",
                                    "data": {
                                        "firmware_version": guard.firmware_version.clone(),
                                        "active_sim": guard.active_sim.clone(),
                                        "network_provider": guard.network_provider.clone(),
                                        "apn": guard.apn.clone(),
                                    }
                                })
                            };
                            let _ = local_tx.send(info_json.to_string()).await;
                            continue;
                        }
                        "get_backend_log" => {
                            let logs = get_logs_snapshot().await;
                            let log_json = serde_json::json!({
                                "type": "backend_log",
                                "data": logs
                            });
                            let _ = local_tx.send(log_json.to_string()).await;
                            continue;
                        }
                        "set_apn" => {
                            let payload = cmd.payload.as_ref();
                            let apn = payload.and_then(|p| p.get("apn")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let user = payload.and_then(|p| p.get("user")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let pass = payload.and_then(|p| p.get("pass")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let auth = payload.and_then(|p| p.get("auth")).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
                            AtAction::SetApn(apn, user, pass, auth)
                        }
                        "set_network_mode" => {
                            let mode = cmd.payload.as_ref().and_then(|p| p.as_str()).unwrap_or("auto").to_string();
                            AtAction::SetNetworkMode(mode)
                        }
                        "net_connect" => AtAction::NetConnect(true),
                        "net_disconnect" => AtAction::NetConnect(false),
                        "network_scan" => AtAction::NetworkScan,
                        "send_sms" => {
                            let payload = cmd.payload.as_ref();
                            let recipient = payload.and_then(|p| p.get("recipient")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let message = payload.and_then(|p| p.get("message")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            AtAction::SendSms(recipient, message)
                        }
                        "get_sms_list" => AtAction::GetSmsList,
                        "get_device_info" => AtAction::GetDeviceInfo,
                        "reboot" => AtAction::Reboot,
                        "factory_reset" => AtAction::FactoryReset,
                        "flight_mode" => {
                            let on = cmd.payload.as_ref().and_then(|p| p.as_str()).unwrap_or("0") == "1";
                            AtAction::FlightMode(on)
                        }
                        "set_band_lock" => {
                            let payload = cmd.payload.as_ref();
                            let is_nr5g = payload.and_then(|p| p.get("nr5g")).or_else(|| payload.and_then(|p| p.get("is_nr5g"))).and_then(|v| v.as_bool()).unwrap_or(false);
                            let bands = payload.and_then(|p| p.get("bands")).or_else(|| payload.and_then(|p| p.get("band"))).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            AtAction::SetBandLock { is_nr5g, bands }
                        }
                        "set_cell_lock" => {
                            let payload = cmd.payload.as_ref();
                            let tech = payload.and_then(|p| p.get("tech")).and_then(|v| v.as_str()).unwrap_or("lte").to_string();
                            let pci = payload.and_then(|p| p.get("pci")).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let earfcn = payload.and_then(|p| p.get("earfcn")).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let band = payload.and_then(|p| p.get("band")).and_then(|v| v.as_u64()).map(|v| v as u32);
                            let enable = payload.and_then(|p| p.get("enable")).and_then(|v| v.as_bool()).unwrap_or(true);
                            AtAction::SetCellLock { tech, pci, earfcn, band, enable }
                        }
                        "get_diagnostics" => {
                            let sub = cmd.payload.as_ref().and_then(|p| p.as_str()).unwrap_or("");
                            let diag = match sub {
                                "neighbour" | "neighbor" => DiagnosticType::Neighbour,
                                "qlt" | "qlts" => DiagnosticType::Qlts,
                                "mbn_list" | "qmbncfg" => DiagnosticType::MbnList,
                                "autosel_query" | "autosel" => DiagnosticType::AutoSelQuery,
                                _ => {
                                    push_log("WARN", "WS", &format!("未知诊断子命令: {}", sub));
                                    continue;
                                }
                            };
                            AtAction::GetDiagnostics(diag)
                        }
                        "set_mode_pref" => {
                            let mode = cmd.payload.as_ref().and_then(|p| p.as_str()).unwrap_or("auto").to_string();
                            AtAction::SetNetworkMode(mode)
                        }
                        "set_usb_net_mode" => {
                            let mode = cmd.payload.as_ref()
                                .and_then(|p| p.as_u64().or_else(|| p.as_str().and_then(|s| s.parse::<u64>().ok())))
                                .unwrap_or(0) as u8;
                            AtAction::SetUsbNetMode(mode)
                        }
                        "get_usb_config" => AtAction::GetUsbConfig,
                        "set_sim_slot" => {
                            let slot = cmd.payload.as_ref()
                                .and_then(|p| p.get("slot"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32;
                            AtAction::SetSimSlot(slot)
                        }
                        "get_mbn_list" => AtAction::GetMbnList,
                        "set_mbn" => {
                            let name = cmd.payload.as_ref()
                                .and_then(|p| p.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            AtAction::SetMbn(name)
                        }
                        "mbn_autosel" => {
                            let on = cmd.payload.as_ref().and_then(|p| p.as_str()).unwrap_or("0") == "1";
                            AtAction::SetMbnAutoSel(on)
                        }
                        "mbn_deactivate" => AtAction::DeactivateMbn,
                        "set_eth_config" => {
                            let payload = cmd.payload.as_ref();
                            let driver = payload.and_then(|p| p.get("driver"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("r8125")
                                .to_string();
                            let pcie_rc = payload.and_then(|p| p.get("pcie_rc"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(true);
                            AtAction::SetEthConfig { driver, pcie_rc }
                        }
                        "set_ippt_config" => {
                            let mode = cmd.payload.as_ref()
                                .and_then(|p| p.get("mode"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("dmz")
                                .to_string();
                            AtAction::SetIpptConfig { mode }
                        }
                        unknown_action => {
                            push_log("WARN", "WS", &format!("未知 WebSocket 动作: {:?}", unknown_action));
                            continue;
                        }
                    };

                    if state_inner.command_tx.send(AtRequest { action, resp_tx }).await.is_ok() {
                        if let Ok(reply) = resp_rx.await {
                            if local_tx.send(reply.to_string()).await.is_err() {
                                break;
                            }
                        }
                    }
                } else {
                    push_log("WARN", "WS", &format!("WS 收到无法解析的消息: {:?}", text));
                }
            }
            push_log("INFO", "WS", "浏览器主动断开 WebSocket 连接（接收流正常结束）");
        } => {}
    };

    // _guard.drop() 自动根据 is_view_active 标志决定是否递减

    push_log(
        "INFO",
        "WS",
        &format!(
            "WebSocket 客户端已断开 (当前在线: {})",
            state.tx.receiver_count()
        ),
    );
}
