//! 宿主系统监控：直接读取 Linux `/proc` 与 `/sys`，零堆内存开销。

use std::fs;

/// 直接读取 Linux /proc/uptime，零堆内存开销
pub fn get_uptime_mins() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|content| content.split_whitespace().next()?.parse::<f64>().ok())
        .map(|secs| (secs / 60.0) as u64)
        .unwrap_or(0)
}

/// 直接读取高通 SoC thermal zone 温度，零堆内存开销
pub fn get_soc_temperature() -> Option<String> {
    let paths = [
        "/sys/class/thermal/thermal_zone0/temp",
        "/sys/class/thermal/thermal_zone1/temp",
    ];
    for path in &paths {
        if let Ok(content) = fs::read_to_string(path) {
            if let Ok(milli_c) = content.trim().parse::<f32>() {
                let val = if milli_c > 1000.0 {
                    milli_c / 1000.0
                } else {
                    milli_c
                };
                return Some(format!("{:.0} °C", val));
            }
        }
    }
    None
}

/// CPU 统计快照，用于计算两次读取间的使用率差值
pub struct CpuSnapshot {
    pub total: u64,
    pub idle: u64,
}

/// 直接读取 /proc/stat 计算 CPU 使用率（%），零堆内存开销
/// prev 由调用者维护，消除全局锁
pub fn get_cpu_usage(prev: &mut Option<CpuSnapshot>) -> Option<String> {
    let content = fs::read_to_string("/proc/stat").ok()?;
    let first_line = content.lines().next()?;
    if !first_line.starts_with("cpu ") {
        return None;
    }
    // 单次迭代直接计算 total 和 idle，零堆分配
    let mut total: u64 = 0;
    let mut idle: u64 = 0;
    for (i, val) in first_line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse::<u64>().ok())
        .enumerate()
    {
        total += val;
        match i {
            3 => idle += val, // idle
            4 => idle += val, // iowait (也计入空闲)
            _ => {}
        }
    }
    if total == 0 {
        return None;
    }

    match prev.as_ref() {
        Some(prev_snap) => {
            let total_delta = total.wrapping_sub(prev_snap.total);
            let idle_delta = idle.wrapping_sub(prev_snap.idle);
            *prev = Some(CpuSnapshot { total, idle });
            if total_delta > 0 {
                let usage =
                    total_delta.saturating_sub(idle_delta) as f64 / total_delta as f64 * 100.0;
                Some(format!("{:.1}%", usage))
            } else {
                None
            }
        }
        None => {
            *prev = Some(CpuSnapshot { total, idle });
            None
        }
    }
}

/// 直接读取 /proc/meminfo 计算内存使用率，零堆内存开销
pub fn get_memory_usage() -> Option<String> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    let mut mem_total = 0u64;
    let mut mem_avail = 0u64;
    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            if let Some(v) = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()) {
                mem_total = v;
            }
        } else if line.starts_with("MemAvailable:") {
            if let Some(v) = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()) {
                mem_avail = v;
            }
        }
    }
    if mem_total == 0 || mem_avail == 0 {
        return None;
    }
    let used = mem_total.saturating_sub(mem_avail);
    let pct = used as f64 / mem_total as f64 * 100.0;
    let used_mb = used as f64 / 1024.0;
    let total_mb = mem_total as f64 / 1024.0;
    Some(format!("{:.1}% ({:.0}M/{:.0}M)", pct, used_mb, total_mb))
}
