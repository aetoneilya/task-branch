use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "wt-plugin-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn wt(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_wt"));
        cmd.current_dir(&self.0)
            .env("XDG_CONFIG_HOME", &self.0)
            .env_remove("WORKTABLE_CONFIG")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
            .env_remove("HERDR_PLUGIN_CONFIG_DIR");
        cmd
    }

    fn config(&self, name: &str, project: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("[vault]\npath = '/unused'\n[[project]]\nname = '{project}'\ntask_dirs = ['tasks']\n"),
        )
        .unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn script(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn action_uses_host_binary_without_requiring_config() {
    let f = Fixture::new();
    let host = f.path("host with spaces");
    script(
        &host,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$WT_TEST_ARGS\"\nprintf '%s\\n' '{\"result\":{\"type\":\"ok\"}}'\n",
    );
    let output = f
        .wt()
        .arg("plugin-open")
        .env("HERDR_BIN_PATH", host)
        .env("WT_TEST_ARGS", f.path("args"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(f.path("args")).unwrap(),
        "plugin\npane\nopen\n--plugin\nworktable\n--entrypoint\nboard\n"
    );
}

#[test]
fn plugin_config_overrides_standalone_but_explicit_config_wins() {
    let f = Fixture::new();
    f.config("worktable/config.toml", "standalone");
    f.config("plugin/config.toml", "plugin");
    let explicit = f.config("explicit.toml", "explicit");
    for (override_path, expected) in [(None, "plugin"), (Some(explicit), "explicit")] {
        let mut cmd = f.wt();
        cmd.args(["-p", "missing", "status", "--no-write"])
            .env("HERDR_PLUGIN_CONFIG_DIR", f.path("plugin"));
        if let Some(path) = override_path {
            cmd.env("WORKTABLE_CONFIG", path);
        }
        let output = cmd.output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains(&format!("no project \"missing\": {expected}"))
        );
    }
}

#[test]
fn empty_plugin_directory_keeps_standalone_config() {
    let f = Fixture::new();
    f.config("worktable/config.toml", "standalone");
    let output = f
        .wt()
        .args(["-p", "missing", "status", "--no-write"])
        .env("HERDR_PLUGIN_CONFIG_DIR", f.path("empty-plugin"))
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("no project \"missing\": standalone"));
}

#[test]
fn popup_starts_bundled_binary_with_popup_mode() {
    let f = Fixture::new();
    script(
        &f.path("target/release/wt"),
        "#!/bin/sh\nprintf '%s\\n' \"$WT_POPUP\" \"$1\" \"$HERDR_PLUGIN_CONTEXT_JSON\"\n",
    );
    let output = Command::new("sh")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/popup.sh"))
        .env("SHELL", "/bin/sh")
        .env("HERDR_PLUGIN_ROOT", &f.0)
        .env(
            "HERDR_PLUGIN_CONTEXT_JSON",
            "{\"focused_pane_cwd\":\"/caller\"}",
        )
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "1\ntui\n{\"focused_pane_cwd\":\"/caller\"}\n"
    );
}
