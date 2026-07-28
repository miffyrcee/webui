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
