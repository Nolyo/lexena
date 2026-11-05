//! Check the exact shipped bridge image before an installer replaces it.
use std::path::Path;

pub fn bridge_is_running(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let expected = path
        .canonicalize()
        .map_err(|_| "MCP_BRIDGE_PATH_UNAVAILABLE".to_string())?;
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    Ok(system
        .processes()
        .values()
        .any(|process| match process.exe() {
            Some(executable) => executable.canonicalize().is_ok_and(|exe| exe == expected),
            None => path.file_name().is_some_and(|name| name == process.name()),
        }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn detects_a_real_running_image_without_matching_an_unstarted_copy() {
        let executable = std::env::current_exe().unwrap();
        assert!(super::bridge_is_running(&executable).unwrap());
        let temporary = tempfile::tempdir().unwrap();
        let copy = temporary.path().join("liaison non démarrée.exe");
        std::fs::copy(&executable, &copy).unwrap();
        assert!(!super::bridge_is_running(&copy).unwrap());
    }
}
