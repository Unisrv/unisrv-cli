use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::decl::file::ports::Port;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceContainerDeclaration {
    pub image: String,
    pub args: Option<Vec<String>>,
    pub envs: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceServiceDeclaration {
    TCP(u16),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceDeclaration {
    pub name: String,
    pub container: InstanceContainerDeclaration,

    #[serde(default)]
    pub ports: Vec<Port>,

    pub network: Option<String>,

    pub service: Option<InstanceServiceDeclaration>,
}
