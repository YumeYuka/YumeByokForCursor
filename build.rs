// 把版本号、目标平台和构建时间编进二进制，`ybc --version` 可以直接看到。
fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=YBC_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=YBC_BUILD_ID");
}
