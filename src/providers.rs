//! 供应商清单：YAML 读写，以及切换时对 `state.vscdb` 的改动。
//!
//! 按用户选择：Key 以明文存放在 YAML 中（文件名 `providers.yaml`，位于工具目录）。
//! 模型列表由 Cursor 自己请求 `/models`，本工具只负责切换端点、Key 与缓存。

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::crypto;
use crate::db::Database;
use crate::paths::Paths;

/// 配置里新增字段时保持向后兼容
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// 写入 `featureModelConfigs.composer.defaultModel`；空则不改
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// 切换时是否清空模型缓存
    #[serde(default, skip_serializing_if = "is_false")]
    pub clear_catalog: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProviderBook {
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
}

impl ProviderBook {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_yaml::from_str(&text).map_err(|e| format!("解析 {} 失败: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建目录 {} 失败: {e}", parent.display()))?;
        }
        let text = serde_yaml::to_string(self).map_err(|e| format!("序列化失败: {e}"))?;
        std::fs::write(path, text.as_bytes())
            .map_err(|e| format!("写入 {} 失败: {e}", path.display()))
    }

    pub fn get(&self, name: &str) -> Result<&Provider, String> {
        self.providers
            .get(name)
            .ok_or_else(|| format!("没有名为 {name} 的供应商；可用 list 查看"))
    }

    pub fn names(&self) -> Vec<&String> {
        self.providers.keys().collect()
    }
}

// ---------------------------------------------------------------- 数据库读写

const KEY_APPLICATION_USER: &str =
    "src.vs.platform.reactivestorage.browser.reactiveStorageServiceImpl.persistentStorage.applicationUser";
const KEY_CATALOG: &str = "cursor/localProviderModelCatalog";
const KEY_SECRET: &str = "secret://cursorAuth/openAIKey";

/// 当前 Cursor 实际生效的 BYOK 配置
#[derive(Debug)]
pub struct CurrentConfig {
    pub base_url: Option<String>,
    pub use_open_ai_key: bool,
    pub api_key: Option<String>,
    pub api_key_error: Option<String>,
    pub model_count: usize,
    pub composer_model: Option<String>,
    pub catalog_base_url: Option<String>,
}

pub fn read_current(paths: &Paths) -> Result<CurrentConfig, String> {
    let db_path = paths.state_db_path();
    let db = Database::open(&db_path, false)?;

    let app_text = db
        .get(KEY_APPLICATION_USER)?
        .ok_or("状态库里没有 applicationUser 配置")?;
    let app: Value = serde_json::from_str(&app_text).map_err(|e| format!("解析配置失败: {e}"))?;

    let base_url = app
        .get("openAIBaseUrl")
        .and_then(Value::as_str)
        .map(str::to_string);

    let use_open_ai_key = app.get("useOpenAIKey").and_then(Value::as_bool).unwrap_or(false);

    let model_count = app
        .get("localProviderModelIds")
        .and_then(Value::as_array)
        .map(|v| v.len())
        .unwrap_or(0);

    let composer_model = app
        .pointer("/featureModelConfigs/composer/defaultModel")
        .and_then(Value::as_str)
        .map(str::to_string);

    let catalog_base_url = db
        .get(KEY_CATALOG)?
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .get("baseUrl")
                .and_then(Value::as_str)
                .map(str::to_string)
        });

    let (api_key, api_key_error) = match db.get(KEY_SECRET)? {
        Some(secret) => match crypto::load_master_key(&paths.local_state_path()) {
            Ok(master) => match crypto::decrypt_api_key(&secret, &master) {
                Ok(key) => (Some(key.to_string()), None),
                Err(err) => (None, Some(err)),
            },
            Err(err) => (None, Some(err)),
        },
        None => (None, Some("状态库里没有已保存的 Key".into())),
    };

    Ok(CurrentConfig {
        base_url,
        use_open_ai_key,
        api_key,
        api_key_error,
        model_count,
        composer_model,
        catalog_base_url,
    })
}

/// 切换结果，用于 CLI 汇报
#[derive(Debug)]
pub struct SwitchReport {
    pub backup: PathBuf,
    pub base_url: String,
    pub key_written: bool,
    pub catalog_cleared: bool,
    pub default_model_set: Option<String>,
}

/// 把指定供应商写入 Cursor 配置。调用方需先确认 Cursor 已退出。
pub fn switch_to(paths: &Paths, provider: &Provider, clear_catalog: bool) -> Result<SwitchReport, String> {
    let db_path = paths.state_db_path();
    let backup = backup_database(paths, &db_path)?;

    let db = Database::open(&db_path, true)?;

    let app_text = db
        .get(KEY_APPLICATION_USER)?
        .ok_or("状态库里没有 applicationUser 配置")?;
    let mut app: Value =
        serde_json::from_str(&app_text).map_err(|e| format!("解析配置失败: {e}"))?;

    let obj = app
        .as_object_mut()
        .ok_or("applicationUser 不是 JSON 对象，拒绝改写")?;
    obj.insert("openAIBaseUrl".into(), Value::String(provider.base_url.clone()));
    obj.insert("useOpenAIKey".into(), Value::Bool(true));

    let mut default_model_set = None;
    if let Some(model) = provider.default_model.as_deref().filter(|m| !m.trim().is_empty()) {
        if !app
            .pointer("/featureModelConfigs/composer")
            .map(Value::is_object)
            .unwrap_or(false)
        {
            ensure_object_path(&mut app, &["featureModelConfigs", "composer"])?;
        }
        if let Some(composer) = app.pointer_mut("/featureModelConfigs/composer") {
            if let Some(map) = composer.as_object_mut() {
                map.insert("defaultModel".into(), Value::String(model.to_string()));
                default_model_set = Some(model.to_string());
            }
        }
    }

    let rewritten = serde_json::to_string(&app).map_err(|e| format!("序列化配置失败: {e}"))?;
    db.set(KEY_APPLICATION_USER, &rewritten)?;

    let mut key_written = false;
    if let Some(key) = provider.api_key.as_deref().filter(|k| !k.trim().is_empty()) {
        let master = crypto::load_master_key(&paths.local_state_path())?;
        let blob = crypto::encrypt_blob(key.trim(), &master)?;
        db.set(KEY_SECRET, &crypto::blob_to_store_json(&blob))?;
        key_written = true;
    }

    let should_clear = clear_catalog || provider.clear_catalog;
    if should_clear {
        db.delete(KEY_CATALOG)?;
    }

    Ok(SwitchReport {
        backup,
        base_url: provider.base_url.clone(),
        key_written,
        catalog_cleared: should_clear,
        default_model_set,
    })
}

fn ensure_object_path(root: &mut Value, path: &[&str]) -> Result<(), String> {
    let mut cursor = root;
    for segment in path {
        let map = cursor
            .as_object_mut()
            .ok_or_else(|| format!("路径 {segment} 的父级不是对象"))?;
        cursor = map
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    Ok(())
}

pub fn backup_database(paths: &Paths, db_path: &Path) -> Result<PathBuf, String> {
    let dir = paths.db_backup_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败: {e}"))?;

    let name = format!("state.vscdb.{}.bak", file_timestamp());
    let target = dir.join(name);

    // WAL 模式下单独复制主库可能丢最近事务，这里同时备份 -wal / -shm
    std::fs::copy(db_path, &target)
        .map_err(|e| format!("备份 {} 失败: {e}", db_path.display()))?;

    for suffix in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{suffix}", db_path.display()));
        if from.is_file() {
            let to = PathBuf::from(format!("{}{suffix}", target.display()));
            let _ = std::fs::copy(&from, &to);
        }
    }

    Ok(target)
}

fn file_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

/// 检查是否有 Cursor 进程在跑（只读，不做任何修改）。
///
/// 直接走 Win32 进程快照，避免每次 spawn `tasklist` 额外付出上百毫秒。
pub fn cursor_running() -> Option<Vec<String>> {
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;
    const MAX_PATH: usize = 260;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; MAX_PATH],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;

    let mut found = Vec::new();
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while ok {
        let name = wide_to_ascii(&entry.sz_exe_file);
        if name.eq_ignore_ascii_case("cursor.exe")
            || name.eq_ignore_ascii_case("cursor private inference.exe")
        {
            found.push(name);
        }
        ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }

    unsafe { CloseHandle(snapshot) };

    if found.is_empty() {
        None
    } else {
        Some(found)
    }
}

fn wide_to_ascii(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_roundtrip() {
        let mut book = ProviderBook::default();
        book.active = Some("a".into());
        book.providers.insert(
            "a".into(),
            Provider {
                base_url: "https://api.example.com/v1".into(),
                api_key: Some("sk-1".into()),
                default_model: Some("glm-5.3".into()),
                clear_catalog: true,
                note: None,
            },
        );

        // 测试环境下 %TEMP% 可能不可写，统一落在构建目录里
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-artifacts");
        let dir = base.join(format!("yaml-{}", std::process::id()));
        let path = dir.join("providers.yaml");
        let _ = std::fs::remove_dir_all(&dir);
        book.save(&path).unwrap();

        let loaded = ProviderBook::load(&path).unwrap();
        let provider = loaded.get("a").unwrap();
        assert_eq!(provider.base_url, "https://api.example.com/v1");
        assert_eq!(provider.api_key.as_deref(), Some("sk-1"));
        assert!(provider.clear_catalog);
        assert_eq!(loaded.active.as_deref(), Some("a"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_empty_book() {
        let book = ProviderBook::load(Path::new("C:/definitely/not/here.yaml")).unwrap();
        assert!(book.providers.is_empty());
    }

    #[test]
    fn missing_provider_reports_name() {
        let book = ProviderBook::default();
        let err = book.get("nope").unwrap_err();
        assert!(err.contains("nope"));
    }
}
