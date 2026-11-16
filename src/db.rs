//! 通过 Windows 自带的 `winsqlite3.dll` 读写 `state.vscdb`。
//!
//! 不引入 SQLite crate，也不需要 `sqlite3.exe`：系统 DLL 就足够，
//! 而且用运行时加载，不依赖导入库。所有对外类型都是安全封装。

use std::ffi::{c_char, c_int, c_void, CString};
use std::path::Path;
use std::ptr;

use crate::paths::to_file_uri;

type SqliteHandle = *mut c_void;
type StmtHandle = *mut c_void;

extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_URI: c_int = 0x0000_0040;

struct Api {
    open_v2: unsafe extern "system" fn(*const c_char, *mut SqliteHandle, c_int, *const c_char) -> c_int,
    close: unsafe extern "system" fn(SqliteHandle) -> c_int,
    errmsg: unsafe extern "system" fn(SqliteHandle) -> *const c_char,
    prepare_v2: unsafe extern "system" fn(
        SqliteHandle,
        *const c_char,
        c_int,
        *mut StmtHandle,
        *mut *const c_char,
    ) -> c_int,
    step: unsafe extern "system" fn(StmtHandle) -> c_int,
    finalize: unsafe extern "system" fn(StmtHandle) -> c_int,
    column_text: unsafe extern "system" fn(StmtHandle, c_int) -> *const u8,
    column_bytes: unsafe extern "system" fn(StmtHandle, c_int) -> c_int,
    bind_text: unsafe extern "system" fn(StmtHandle, c_int, *const c_char, c_int, *const c_void) -> c_int,
}

impl Api {
    fn load() -> Result<&'static Api, String> {
        static ONCE: std::sync::OnceLock<Result<Api, String>> = std::sync::OnceLock::new();
        ONCE.get_or_init(Api::try_load).as_ref().map_err(|e| e.clone())
    }

    fn try_load() -> Result<Api, String> {
        let wide: Vec<u16> = "winsqlite3.dll\0".encode_utf16().collect();
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            return Err("无法加载 winsqlite3.dll（需要 Windows 10 及以上）".into());
        }

        unsafe fn sym(module: *mut c_void, name: &str) -> Result<*mut c_void, String> {
            let cname = CString::new(name).unwrap();
            let p = unsafe { GetProcAddress(module, cname.as_ptr()) };
            if p.is_null() {
                return Err(format!("winsqlite3.dll 缺少导出 {name}"));
            }
            Ok(p)
        }

        unsafe {
            Ok(Api {
                open_v2: std::mem::transmute(sym(module, "sqlite3_open_v2")?),
                close: std::mem::transmute(sym(module, "sqlite3_close")?),
                errmsg: std::mem::transmute(sym(module, "sqlite3_errmsg")?),
                prepare_v2: std::mem::transmute(sym(module, "sqlite3_prepare_v2")?),
                step: std::mem::transmute(sym(module, "sqlite3_step")?),
                finalize: std::mem::transmute(sym(module, "sqlite3_finalize")?),
                column_text: std::mem::transmute(sym(module, "sqlite3_column_text")?),
                column_bytes: std::mem::transmute(sym(module, "sqlite3_column_bytes")?),
                bind_text: std::mem::transmute(sym(module, "sqlite3_bind_text")?),
            })
        }
    }
}

pub struct Database {
    handle: SqliteHandle,
}

impl Database {
    pub fn open(path: &Path, writable: bool) -> Result<Self, String> {
        let api = Api::load()?;
        let uri = CString::new(to_file_uri(path, !writable)).map_err(|e| e.to_string())?;
        let flags = SQLITE_OPEN_URI | if writable { SQLITE_OPEN_READWRITE } else { SQLITE_OPEN_READONLY };

        let mut handle: SqliteHandle = ptr::null_mut();
        let rc = unsafe { (api.open_v2)(uri.as_ptr(), &mut handle, flags, ptr::null()) };
        if rc != SQLITE_OK {
            let msg = if handle.is_null() {
                format!("sqlite3_open_v2 失败，code={rc}")
            } else {
                unsafe { cstr((api.errmsg)(handle)) }
            };
            if !handle.is_null() {
                unsafe { (api.close)(handle) };
            }
            return Err(format!("打开 {} 失败: {msg}", path.display()));
        }
        Ok(Self { handle })
    }

    /// 读单行单列；不存在返回 `None`。
    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        let api = Api::load()?;
        let stmt = self.prepare(api, "SELECT value FROM ItemTable WHERE key = ?1")?;
        let guard = StmtGuard { handle: stmt };
        self.bind_text(api, stmt, key)?;

        let result = match unsafe { (api.step)(stmt) } {
            SQLITE_ROW => Ok(Some(self.column_text(api, stmt, 0))),
            SQLITE_DONE => Ok(None),
            rc => Err(self.error(api, format!("查询 {key} 失败，code={rc}"))),
        };
        drop(guard);
        result
    }

    pub fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.with_transaction(|db| {
            let api = Api::load()?;
            let stmt = db.prepare(
                api,
                "INSERT INTO ItemTable(key, value) VALUES(?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )?;
            let guard = StmtGuard { handle: stmt };
            db.bind_text(api, stmt, key)?;
            db.bind_index(api, stmt, 2, value)?;
            match unsafe { (api.step)(stmt) } {
                SQLITE_DONE => {}
                rc => return Err(db.error(api, format!("写入 {key} 失败，code={rc}"))),
            }
            drop(guard);
            Ok(())
        })
    }

    pub fn delete(&self, key: &str) -> Result<(), String> {
        self.with_transaction(|db| {
            let api = Api::load()?;
            let stmt = db.prepare(api, "DELETE FROM ItemTable WHERE key = ?1")?;
            let guard = StmtGuard { handle: stmt };
            db.bind_text(api, stmt, key)?;
            let rc = unsafe { (api.step)(stmt) };
            drop(guard);
            if rc == SQLITE_DONE {
                Ok(())
            } else {
                Err(db.error(api, format!("删除 {key} 失败，code={rc}")))
            }
        })
    }

    fn with_transaction<T, F>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&Self) -> Result<T, String>,
    {
        self.exec("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(value) => {
                self.exec("COMMIT")?;
                Ok(value)
            }
            Err(err) => {
                let _ = self.exec("ROLLBACK");
                Err(err)
            }
        }
    }

    fn exec(&self, sql: &str) -> Result<(), String> {
        let api = Api::load()?;
        let stmt = self.prepare(api, sql)?;
        let guard = StmtGuard { handle: stmt };
        let rc = unsafe { (api.step)(stmt) };
        drop(guard);
        if rc == SQLITE_DONE || rc == SQLITE_ROW {
            Ok(())
        } else {
            Err(self.error(api, format!("执行 {sql} 失败，code={rc}")))
        }
    }

    fn prepare(&self, api: &Api, sql: &str) -> Result<StmtHandle, String> {
        let c_sql = CString::new(sql).map_err(|e| e.to_string())?;
        let mut stmt: StmtHandle = ptr::null_mut();
        let rc = unsafe { (api.prepare_v2)(self.handle, c_sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if rc != SQLITE_OK {
            return Err(self.error(api, format!("prepare 失败，code={rc}")));
        }
        Ok(stmt)
    }

    fn bind_text(&self, api: &Api, stmt: StmtHandle, key: &str) -> Result<(), String> {
        self.bind_index(api, stmt, 1, key)
    }

    fn bind_index(&self, api: &Api, stmt: StmtHandle, index: c_int, text: &str) -> Result<(), String> {
        let rc = unsafe {
            (api.bind_text)(stmt, index, text.as_ptr().cast(), text.len() as c_int, ptr::null())
        };
        if rc == SQLITE_OK {
            Ok(())
        } else {
            Err(self.error(api, format!("bind 失败，code={rc}")))
        }
    }

    fn column_text(&self, api: &Api, stmt: StmtHandle, index: c_int) -> String {
        unsafe {
            let ptr = (api.column_text)(stmt, index);
            let len = (api.column_bytes)(stmt, index);
            if ptr.is_null() || len <= 0 {
                return String::new();
            }
            let slice = std::slice::from_raw_parts(ptr, len as usize);
            String::from_utf8_lossy(slice).into_owned()
        }
    }

    fn error(&self, api: &Api, prefix: String) -> String {
        let detail = unsafe { cstr((api.errmsg)(self.handle)) };
        if detail.is_empty() {
            prefix
        } else {
            format!("{prefix}: {detail}")
        }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            if let Ok(api) = Api::load() {
                unsafe { (api.close)(self.handle) };
            }
            self.handle = ptr::null_mut();
        }
    }
}

struct StmtGuard {
    handle: StmtHandle,
}

impl Drop for StmtGuard {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            if let Ok(api) = Api::load() {
                unsafe { (api.finalize)(self.handle) };
            }
        }
    }
}

unsafe fn cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}
