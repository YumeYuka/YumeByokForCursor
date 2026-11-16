//! Cursor 的 API Key 存储格式：Chromium v10 封装 + Windows DPAPI 主密钥。
//!
//! ```text
//! blob  = "v10" || nonce(12) || AES-256-GCM(key, nonce, plaintext) || tag(16)
//! store = {"type":"Buffer","data":[int, ...]}
//! key   = DPAPI(Local State 的 os_crypt.encrypted_key，去掉 "DPAPI" 前缀)
//! ```

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::json;
use zeroize::Zeroizing;

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

const V10_PREFIX: &[u8; 3] = b"v10";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[link(name = "crypt32")]
extern "system" {
    fn CryptUnprotectData(
        data_in: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

/// 用 DPAPI（CurrentUser）解包一段受保护的字节。
pub fn dpapi_unprotect(data: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut input = data.to_vec();
    let blob_in = DataBlob { cb_data: input.len() as u32, pb_data: input.as_mut_ptr() };
    let mut blob_out = DataBlob { cb_data: 0, pb_data: std::ptr::null_mut() };

    let ok = unsafe {
        CryptUnprotectData(
            &blob_in,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut blob_out,
        )
    };
    if ok == 0 {
        return Err("DPAPI 解密失败（可能不是同一 Windows 用户创建的）".into());
    }

    let out = unsafe {
        let slice = std::slice::from_raw_parts(blob_out.pb_data, blob_out.cb_data as usize);
        let copied = slice.to_vec();
        LocalFree(blob_out.pb_data.cast());
        copied
    };
    Ok(Zeroizing::new(out))
}

/// 从 `Local State` 取出 DPAPI 加密的主密钥并解包。
pub fn load_master_key(local_state: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    let text = std::fs::read_to_string(local_state)
        .map_err(|e| format!("读取 {} 失败: {e}", local_state.display()))?;
    let root: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析 Local State 失败: {e}"))?;

    let encoded = root
        .get("os_crypt")
        .and_then(|v| v.get("encrypted_key"))
        .and_then(|v| v.as_str())
        .ok_or("Local State 里没有 os_crypt.encrypted_key")?;

    let raw = B64.decode(encoded).map_err(|e| format!("os_crypt.encrypted_key 不是合法 Base64: {e}"))?;
    if raw.len() < 6 || &raw[..5] != b"DPAPI" {
        return Err("os_crypt.encrypted_key 不是 DPAPI 保护格式".into());
    }
    dpapi_unprotect(&raw[5..])
}

/// 把 state.vscdb 里的 `secret://cursorAuth/openAIKey` JSON 解成明文 Key。
pub fn decrypt_api_key(store_json: &str, master_key: &[u8]) -> Result<Zeroizing<String>, String> {
    let root: serde_json::Value =
        serde_json::from_str(store_json).map_err(|e| format!("密钥 JSON 解析失败: {e}"))?;

    let kind = root.get("type").and_then(|v| v.as_str()).unwrap_or_default();
    if kind != "Buffer" {
        return Err(format!("未知的密钥封装类型: {kind}（期望 Buffer）"));
    }

    let data = root
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or("密钥 JSON 缺少 data 数组")?;

    let mut blob = Vec::with_capacity(data.len());
    for item in data {
        let byte = item.as_u64().ok_or("密钥 data 里存在非数字元素")?;
        blob.push(u8::try_from(byte).map_err(|_| "密钥 data 元素超出 0..=255")?);
    }

    decrypt_blob(&blob, master_key)
}

pub fn decrypt_blob(blob: &[u8], master_key: &[u8]) -> Result<Zeroizing<String>, String> {
    if blob.len() < V10_PREFIX.len() + NONCE_LEN + TAG_LEN {
        return Err("密钥 blob 太短".into());
    }
    if &blob[..3] != V10_PREFIX {
        return Err("不支持的密钥封装前缀（期望 v10）".into());
    }

    let nonce = &blob[3..3 + NONCE_LEN];
    let cipher_and_tag = &blob[3 + NONCE_LEN..];
    let (cipher, tag) = cipher_and_tag.split_at(cipher_and_tag.len() - TAG_LEN);

    let cipher_impl = new_cipher(master_key)?;
    let mut payload = Vec::with_capacity(cipher.len() + TAG_LEN);
    payload.extend_from_slice(cipher);
    payload.extend_from_slice(tag);

    let plain = cipher_impl
        .decrypt(Nonce::from_slice(nonce), payload.as_ref())
        .map_err(|_| "AES-GCM 解密失败（tag 校验未通过）".to_string())?;

    let text = String::from_utf8(plain).map_err(|e| format!("密钥不是合法 UTF-8: {e}"))?;
    Ok(Zeroizing::new(text))
}

/// 生成新的 v10 blob；nonce 随机。
pub fn encrypt_blob(plaintext: &str, master_key: &[u8]) -> Result<Vec<u8>, String> {
    let nonce = random_bytes::<NONCE_LEN>()?;
    encrypt_blob_with_nonce(plaintext, master_key, &nonce)
}

pub fn encrypt_blob_with_nonce(plaintext: &str, master_key: &[u8], nonce: &[u8]) -> Result<Vec<u8>, String> {
    if nonce.len() != NONCE_LEN {
        return Err("nonce 必须是 12 字节".into());
    }

    let cipher_impl = new_cipher(master_key)?;
    let sealed = cipher_impl
        .encrypt(Nonce::from_slice(nonce), plaintext.as_bytes())
        .map_err(|_| "AES-GCM 加密失败".to_string())?;

    let mut blob = Vec::with_capacity(3 + NONCE_LEN + sealed.len());
    blob.extend_from_slice(V10_PREFIX);
    blob.extend_from_slice(nonce);
    blob.extend_from_slice(&sealed);
    Ok(blob)
}

/// 把 v10 blob 包成 Cursor 使用的 `{"type":"Buffer","data":[...]}` JSON。
pub fn blob_to_store_json(blob: &[u8]) -> String {
    let data: Vec<u16> = blob.iter().map(|b| *b as u16).collect();
    json!({ "type": "Buffer", "data": data }).to_string()
}

fn new_cipher(master_key: &[u8]) -> Result<Aes256Gcm, String> {
    Aes256Gcm::new_from_slice(master_key)
        .map_err(|_| format!("主密钥长度不合法（{} 字节），AES-256-GCM 需要 32 字节", master_key.len()))
}

/// 用系统 CSPRNG 生成随机字节。
pub fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut buffer = [0u8; N];
    fill_random(&mut buffer)?;
    Ok(buffer)
}

pub fn fill_random(buffer: &mut [u8]) -> Result<(), String> {
    use std::ffi::c_void;

    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(
            algorithm: *mut c_void,
            buffer: *mut u8,
            len: u32,
            flags: u32,
        ) -> i32;
    }

    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(format!("BCryptGenRandom 失败，code=0x{status:08x}"));
    }
    Ok(())
}

/// 把字符串转成宽字符指针，供需要 C 风格路径的 Win32 调用。
#[allow(dead_code)]
pub fn wide(path: &Path) -> Vec<u16> {
    OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect()
}

/// 校验文件存在；供 CLI 输出友好错误。
#[allow(dead_code)]
pub fn require_file(path: &Path, label: &str) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("{label} 不存在: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_with_fixed_nonce() {
        let key = [7u8; 32];
        let nonce = [9u8; NONCE_LEN];
        let blob = encrypt_blob_with_nonce("sk-test-123", &key, &nonce).unwrap();
        assert_eq!(&blob[..3], b"v10");
        assert_eq!(blob.len(), 3 + NONCE_LEN + 16 + "sk-test-123".len());

        let plain = decrypt_blob(&blob, &key).unwrap();
        assert_eq!(plain.as_str(), "sk-test-123");
    }

    #[test]
    fn rejects_wrong_prefix() {
        let key = [7u8; 32];
        let mut blob = encrypt_blob_with_nonce("x", &key, &[1u8; NONCE_LEN]).unwrap();
        blob[0] = b'v';
        blob[1] = b'1';
        blob[2] = b'1';
        assert!(decrypt_blob(&blob, &key).unwrap_err().contains("v10"));
    }

    #[test]
    fn rejects_empty_master_key() {
        assert!(new_cipher(&[]).is_err());
    }

    #[test]
    fn store_json_roundtrip() {
        let blob = vec![0x76u8, 0x31, 0x30, 1, 2, 3];
        let json = blob_to_store_json(&blob);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "Buffer");
        assert_eq!(parsed["data"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn expects_buffer_type() {
        let err = decrypt_api_key(r#"{"type":"String","data":[]}"#, &[0u8; 32]).unwrap_err();
        assert!(err.contains("封装类型"), "unexpected: {err}");
    }
}
