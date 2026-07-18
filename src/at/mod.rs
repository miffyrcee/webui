pub mod parser;
pub mod response;
pub mod utils;

// 重新导出解析函数，简化 main.rs 的导入
// 重新导出 main.rs 需要的解析函数
pub use parser::{
    parse_cgpaddr,
    parse_cops_scan,
    parse_net_status,
    parse_qcainfo,
    parse_qcfg_bands,
    parse_qeng,
    parse_qtemp_temperature,
    parse_signal_quality,
    parse_traffic_line,
};

// 重新导出 main.rs 需要的工具函数
pub use utils::{
    decode_cmgl_body,
    decode_hex_ucs2,
    normalize_at_command,
};
