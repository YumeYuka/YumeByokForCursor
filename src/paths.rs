//! 路径解析：全部支持环境变量覆盖，便于测试与非常规安装。

use std::path::{Path, PathBuf};

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
    pub fn resolve() -> Result<Self, String> {
        let local = env_path("LOCALAPPDATA").ok_or("无法读取 LOCALAPPDATA")?;
        let roaming = env_path("APPDATA").ok_or("无法读取 APPDATA")?;
        let home = env_path("USERPROFILE").ok_or("无法读取 USERPROFILE")?;

        let app_dir = env_path("YBC_APP_DIR").unwrap_or_else(|| local.join("Programs").join("cursor"));
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
        self.config_dir.join("js-backups")
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
}
