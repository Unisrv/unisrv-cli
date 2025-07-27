use crate::decl::file::UnisrvFile;
use anyhow::Result;
use std::path::Path;

pub fn find_and_load(root: &Path) -> Result<UnisrvFile> {
    let single_file_path = root.join("unisrv.toml");
    if single_file_path.exists() {
        let file_content = std::fs::read_to_string(single_file_path)?;
        return Ok(toml::from_str(&file_content)?);
    }

    Err(anyhow::anyhow!(
        "No unisrv.toml file found in the specified directory: {}",
        root.display()
    )) //TODO: handle multiple files in a directory.
}
