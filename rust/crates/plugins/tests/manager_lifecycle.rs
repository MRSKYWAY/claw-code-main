use plugins::{PluginManager, PluginManagerConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
        fs::create_dir_all(&path).expect("temporary directory should be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_manifest(root: &Path, version: &str) {
    fs::write(
        root.join("plugin.json"),
        format!(
            r#"{{
  "name": "lifecycle-demo",
  "version": "{version}",
  "description": "Plugin lifecycle integration test",
  "permissions": [],
  "defaultEnabled": false
}}"#
        ),
    )
    .expect("plugin manifest should be written");
}

fn manager_root() -> (TempDir, PluginManager) {
    let config_home = TempDir::new("claw-plugin-config");
    let manager = PluginManager::new(PluginManagerConfig::new(config_home.path()));
    (config_home, manager)
}

#[test]
fn install_enable_disable_and_uninstall_are_persistent() {
    let (config_home, mut manager) = manager_root();
    let source = TempDir::new("claw-plugin-source");
    write_manifest(source.path(), "1.0.0");

    let install = manager
        .install(source.path().to_str().expect("utf-8 temp path"))
        .expect("plugin should install");
    assert_eq!(install.version, "1.0.0");

    let listed = manager
        .list_installed_plugins()
        .expect("installed plugins should list");
    assert_eq!(listed.len(), 1);
    assert!(listed[0].enabled);
    assert_eq!(listed[0].metadata.id, install.plugin_id);

    manager
        .disable(&install.plugin_id)
        .expect("plugin should disable");
    let listed = manager
        .list_installed_plugins()
        .expect("disabled plugin should remain listed");
    assert!(!listed[0].enabled);

    manager
        .enable(&install.plugin_id)
        .expect("plugin should re-enable");
    let listed = manager
        .list_installed_plugins()
        .expect("re-enabled plugin should list");
    assert!(listed[0].enabled);

    manager
        .uninstall(&install.plugin_id)
        .expect("plugin should uninstall");
    assert!(manager
        .list_installed_plugins()
        .expect("installed plugins should list after uninstall")
        .is_empty());
    assert!(!install.install_path.exists());
    assert!(config_home.path().exists());
}

#[test]
fn update_refreshes_version_and_description_without_changing_identity() {
    let (_config_home, mut manager) = manager_root();
    let source = TempDir::new("claw-plugin-source");
    write_manifest(source.path(), "1.0.0");

    let install = manager
        .install(source.path().to_str().expect("utf-8 temp path"))
        .expect("plugin should install");

    write_manifest(source.path(), "2.0.0");
    fs::write(
        source.path().join("plugin.json"),
        r#"{
  "name": "lifecycle-demo",
  "version": "2.0.0",
  "description": "Updated plugin lifecycle integration test",
  "permissions": [],
  "defaultEnabled": false
}"#,
    )
    .expect("updated manifest should be written");

    let update = manager
        .update(&install.plugin_id)
        .expect("plugin should update");

    assert_eq!(update.plugin_id, install.plugin_id);
    assert_eq!(update.old_version, "1.0.0");
    assert_eq!(update.new_version, "2.0.0");
    assert_eq!(update.install_path, install.install_path);

    let listed = manager
        .list_installed_plugins()
        .expect("updated plugin should list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].metadata.version, "2.0.0");
    assert_eq!(
        listed[0].metadata.description,
        "Updated plugin lifecycle integration test"
    );
}
