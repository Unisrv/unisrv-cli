pub mod instance;
pub mod ports;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnisrvFile {
    #[serde(default)]
    #[serde(rename = "instance")]
    pub instances: Vec<instance::InstanceDeclaration>,
}
