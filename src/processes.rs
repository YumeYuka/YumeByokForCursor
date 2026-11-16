//! Cursor 进程及其实际安装位置。

use std::{ffi::c_void, path::PathBuf};

pub struct CursorProcess {
    pub name: String,
    pub app_dir: Option<PathBuf>,
}

pub fn cursor_processes() -> Vec<CursorProcess> {
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
        sz_exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> *mut c_void;
        fn QueryFullProcessImageNameW(process: *mut c_void, flags: u32, name: *mut u16, size: *mut u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    let snapshot = unsafe { CreateToolhelp32Snapshot(0x0000_0002, 0) };
    if snapshot.is_null() || snapshot == -1isize as *mut c_void {
        return Vec::new();
    }
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
    let mut found = Vec::new();
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while ok {
        let len = entry.sz_exe_file.iter().position(|&c| c == 0).unwrap_or(260);
        let name = String::from_utf16_lossy(&entry.sz_exe_file[..len]);
        if name.eq_ignore_ascii_case("cursor.exe") || name.eq_ignore_ascii_case("cursor private inference.exe") {
            let mut app_dir = None;
            let process = unsafe { OpenProcess(0x1000, 0, entry.th32_process_id) };
            if !process.is_null() {
                let mut buffer = vec![0u16; 32768];
                let mut size = buffer.len() as u32;
                if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size) } != 0 {
                    use std::os::windows::ffi::OsStringExt;
                    let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..size as usize]));
                    app_dir = path.parent().map(PathBuf::from);
                }
                unsafe { CloseHandle(process) };
            }
            found.push(CursorProcess { name, app_dir });
        }
        ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    found
}
