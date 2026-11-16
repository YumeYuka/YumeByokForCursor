//! 路径解析：全部支持环境变量覆盖，便于测试与非常规安装。

use std::{hash::{Hash, Hasher}, path::{Path, PathBuf}};

#[derive(Debug, Clone)]
pub struct Paths {
    /// Cursor 安装根目录，内含 `resources/app`
    pub app_dir: PathBuf,
    /// Cursor 用户数据根目录，内含 `User/globalStorage/state.vscdb`
    pub data_dir: PathBuf,
    /// 配置目录：`~/.config/ybc`，存放供应商清单与备份
    pub config_dir: PathBuf,
    /// 显式覆盖 state.vscdb 路径
    pub state_db: Option<PathBuf>,
}

impl Paths {
    pub fn resolve(for_patch: bool) -> Result<Self, String> {
        let local = env_path("LOCALAPPDATA").ok_or("无法读取 LOCALAPPDATA")?;
        let roaming = env_path("APPDATA").ok_or("无法读取 APPDATA")?;
        let home = env_path("USERPROFILE").ok_or("无法读取 USERPROFILE")?;

        let app_dir = match env_path("YBC_APP_DIR") {
            Some(path) => path,
            None if !for_patch => local.join("Programs").join("cursor"),
            None => {
                let running = crate::processes::cursor_processes()
                    .into_iter().filter_map(|process| process.app_dir).collect();
                let installed = [
                    Some(local.join("Programs").join("cursor")),
                    env_path("ProgramW6432").map(|dir| dir.join("cursor")),
                    env_path("ProgramFiles").map(|dir| dir.join("cursor")),
                    env_path("ProgramFiles(x86)").map(|dir| dir.join("cursor")),
                ].into_iter().flatten().collect();
                select_app_dir(running, installed)?
                    .unwrap_or_else(|| local.join("Programs").join("cursor"))
            }
        };
        let data_dir = env_path("YBC_DATA_DIR").unwrap_or_else(|| roaming.join("Cursor Private Inference"));
        let config_dir = env_path("YBC_CONFIG_DIR")
            .or_else(|| env_path("XDG_CONFIG_HOME").map(|dir| dir.join("ybc")))
            .unwrap_or_else(|| home.join(".config").join("ybc"));
        let state_db = env_path("YBC_STATE_DB");

        Ok(Self { app_dir, data_dir, config_dir, state_db })
    }

    pub fn state_db_path(&self) -> PathBuf {
        self.state_db
            .clone()
            .unwrap_or_else(|| self.data_dir.join("User").join("globalStorage").join("state.vscdb"))
    }

    pub fn local_state_path(&self) -> PathBuf {
        self.data_dir.join("Local State")
    }

    pub fn providers_file(&self) -> PathBuf {
        self.config_dir.join("providers.yaml")
    }

    pub fn js_backup_dir(&self) -> PathBuf {
        // 两份安装不能共用备份，否则 revert 会恢复另一份安装的 bundle。
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        path_key(&self.app_dir).hash(&mut hash);
        self.config_dir.join("js-backups").join(format!("{:016x}", hash.finish()))
    }

    pub fn db_backup_dir(&self) -> PathBuf {
        self.config_dir.join("db-backups")
    }

    /// 需要打补丁的两个扩展 bundle
    pub fn bundles(&self) -> Vec<Bundle> {
        let extensions = self.app_dir.join("resources").join("app").join("extensions");
        vec![
            Bundle {
                tag: "agent-exec",
                path: extensions.join("cursor-agent-exec").join("dist").join("main.js"),
            },
            Bundle {
                tag: "local-agent-runtime",
                path: extensions.join("cursor-local-agent-runtime").join("dist").join("main.js"),
            },
        ]
    }

    /// Cursor 主程序路径（用于提示）
    #[allow(dead_code)]
    pub fn executable(&self) -> PathBuf {
        self.app_dir.join("Cursor Private Inference.exe")
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn select_app_dir(running: Vec<PathBuf>, installed: Vec<PathBuf>) -> Result<Option<PathBuf>, String> {
    for candidates in [running, installed] {
        let mut found: Vec<PathBuf> = Vec::new();
        for path in candidates {
            if path.join("resources").join("app").is_dir()
                && !found.iter().any(|other| path_key(other) == path_key(&path))
            {
                found.push(path);
            }
        }
        match found.len() {
            0 => continue,
            1 => return Ok(found.pop()),
            _ => return Err(format!(
                "检测到多份 Cursor 安装，请设置 $env:YBC_APP_DIR 指定目标目录：\n{}",
                found.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n")
            )),
        }
    }
    Ok(None)
}

#[derive(Debug, Clone)]
pub struct Bundle {
    pub tag: &'static str,
    pub path: PathBuf,
}

impl Bundle {
    pub fn exists(&self) -> bool {
        self.path.is_file()
    }
}

pub fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// 把路径转成 SQLite URI，供 winsqlite3 以只读/读写方式打开。
pub fn to_file_uri(path: &Path, read_only: bool) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file:");
    // 驱动器盘符原样保留（C:/...），其余路径段做百分号转义
    let (prefix, rest) = if raw.len() >= 2 && raw.as_bytes()[1] == b':' {
        raw.split_at(2)
    } else {
        ("", raw.as_str())
    };
    uri.push_str(prefix);
    uri.push_str(&encode_path(rest));
    uri.push('?');
    uri.push_str(if read_only { "mode=ro" } else { "mode=rw" });
    uri
}

fn encode_path(segments: &str) -> String {
    let mut out = String::with_capacity(segments.len() + 8);
    for byte in segments.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_spaces_and_keeps_drive_letter() {
        let uri = to_file_uri(Path::new(r"C:\Users\a b\Cursor Private Inference\state.vscdb"), true);
        assert!(uri.starts_with("file:C:/Users/a%20b/Cursor%20Private%20Inference/state.vscdb?"));
        assert!(uri.ends_with("mode=ro"));
    }

    #[test]
    fn swap_backslashes() {
        let uri = to_file_uri(Path::new(r"D:\x\y.vscdb"), false);
        assert!(uri.starts_with("file:D:/x/y.vscdb?mode=rw"));
    }

    #[test]
    fn selects_running_system_install_and_keeps_backups_separate() {
        let root = std::env::temp_dir().join(format!("ybc-paths-{}", std::process::id()));
        let user = root.join("user");
        let system = root.join("system");
        for dir in [&user, &system] {
            std::fs::create_dir_all(dir.join("resources/app")).unwrap();
        }
        let installed = vec![user.clone(), system.clone()];
        assert_eq!(select_app_dir(vec![system.clone(), system.clone()], installed.clone()).unwrap(), Some(system.clone()));
        assert_eq!(select_app_dir(vec![], vec![system.clone()]).unwrap(), Some(system.clone()));
        assert_eq!(select_app_dir(vec![], vec![user.clone()]).unwrap(), Some(user.clone()));
        assert!(select_app_dir(vec![], installed.clone()).unwrap_err().contains("YBC_APP_DIR"));
        assert!(select_app_dir(installed.clone(), installed).is_err());
        assert_eq!(select_app_dir(vec![], vec![root.join("missing")]).unwrap(), None);

        let mut paths = Paths { app_dir: user, data_dir: root.clone(), config_dir: root.clone(), state_db: None };
        let user_backup = paths.js_backup_dir();
        paths.app_dir = system;
        assert_ne!(user_backup, paths.js_backup_dir());
        std::fs::remove_dir_all(root).unwrap();
    }
}
