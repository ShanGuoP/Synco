//! 工作流映射：字段绑定与原始计算图分离，执行只修改副本。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub node_id: String,
    pub field: String,
    pub role: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub bindings: Vec<Binding>,
    pub output_node: String,
}
