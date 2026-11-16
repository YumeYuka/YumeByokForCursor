//! Cursor BYOK 辅助工具：`ybc`
//!
//! - `apply` / `revert` / `status`：给两个扩展 bundle 打协议与思考等级补丁
//! - `list` / `show` / `add` / `use` / `remove`：供应商清单与切换
//! - `doctor`：环境自检

mod crypto;
mod db;
mod patch;
mod paths;
mod providers;

use std::process::ExitCode;

use patch::{EffortScope, PatchKind, PatchOptions, Patcher};
use paths::Paths;
use providers::{Provider, ProviderBook};

const USAGE: &str = "\
ybc - Cursor BYOK 辅助工具

供应商（日常用这两个）:
  ybc set --base-url <url> --api-key <key> [name]   设置并立即生效
  ybc set --base-url <url>                          只换端点，Key 沿用上次
  ybc set [name] --api-key <key>                    只换 Key
  ybc use <name>                                    切到已保存的供应商
  ybc list                                          当前配置 + 所有供应商
  ybc show <name>                                   看单个供应商
  ybc add <name> --base-url <url> [--api-key <key>] 只存不进 Cursor
  ybc remove <name>                                 删掉

补丁（安装或升级 Cursor 后跑一次）:
  ybc apply                                         打全部补丁
  ybc status                                        看补丁状态
  ybc revert                                        还原
  ybc doctor                                        环境自检

补丁选项:
  --patch <protocol|effort|all>       默认 all
  --host-pattern <host>               协议补丁匹配的 baseUrl 片段
  --effort-scope <all|prefix>         默认 all
  --effort-exclude <regex>            all 模式下排除的模型前缀
  --effort-prefixes <regex>           prefix 模式下生效的模型前缀
  --effort-values <v1,v2,...>         思考等级档位
  --keep-gate                         保留 api_types 前置条件
  --no-node-check                     跳过 node 语法校验

切换选项:
  --keep-catalog                      不清空模型缓存
  --force                             跳过「Cursor 已退出」检查（危险）

  -h, --help                          显示本帮助
  -V, --version                       显示版本
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("ybc {} ({})", env!("CARGO_PKG_VERSION"), env!("YBC_TARGET"));
        return ExitCode::SUCCESS;
    }
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("错误: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let command = args[0].as_str();
    let flags = Flags::parse(&args[1..])?;
    let paths = Paths::resolve()?;

    match command {
        "status" => cmd_status(&paths, &flags),
        "apply" => cmd_apply(&paths, &flags),
        "revert" => cmd_revert(&paths),
        "doctor" => cmd_doctor(&paths),
        "list" => cmd_list(&paths),
        "show" => cmd_show(&paths, &flags),
        "set" => cmd_set(&paths, &flags),
        "add" => cmd_add(&paths, &flags),
        "use" => cmd_use(&paths, &flags),
        "remove" => cmd_remove(&paths, &flags),
        other => Err(format!("未知命令: {other}\n\n{USAGE}")),
    }
}

// ---------------------------------------------------------------- 补丁命令

fn patch_options(flags: &Flags) -> Result<PatchOptions, String> {
    let mut options = PatchOptions::default();
    if let Some(host) = flags.value("host-pattern") {
        options.host_pattern = host.to_string();
    }
    if let Some(scope) = flags.value("effort-scope") {
        options.scope = EffortScope::parse(scope)?;
    }
    if let Some(exclude) = flags.value("effort-exclude") {
        options.exclude = exclude.to_string();
    }
    if let Some(prefixes) = flags.value("effort-prefixes") {
        options.prefixes = prefixes.to_string();
    }
    if let Some(values) = flags.value("effort-values") {
        let parsed: Vec<String> = values
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .collect();
        if parsed.is_empty() {
            return Err("--effort-values 不能为空".into());
        }
        options.values = parsed;
    }
    options.remove_gate = !flags.has("keep-gate");
    options.node_check = !flags.has("no-node-check");
    Ok(options)
}

fn cmd_status(paths: &Paths, flags: &Flags) -> Result<(), String> {
    let options = patch_options(flags)?;
    let backup_dir = paths.js_backup_dir();
    let patcher = Patcher::new(&options, &backup_dir);

    println!("安装目录: {}", paths.app_dir.display());
    println!("备份目录: {}", paths.js_backup_dir().display());
    println!("思考等级: scope={:?} values={}", options.scope, options.values.join(","));

    for bundle in paths.bundles() {
        let status = patcher.status(&bundle);
        println!();
        println!("[{}]", status.tag);
        if !status.exists {
            println!("  文件不存在: {}", status.path.display());
            continue;
        }
        println!("  protocol  {}", status.protocol.label());
        println!("  effort    {}", status.effort.label());
        println!("  path      {}", status.path.display());
        if let Some(sha) = &status.sha256 {
            println!("  sha256    {sha}");
        }    }
    println!();
    Ok(())
}

fn cmd_apply(paths: &Paths, flags: &Flags) -> Result<(), String> {
    let options = patch_options(flags)?;
    let kinds = PatchKind::parse(flags.value("patch").unwrap_or("all"))?;
    let backup_dir = paths.js_backup_dir();
    let patcher = Patcher::new(&options, &backup_dir);

    println!("补丁: {:?}", kinds);
    let mut changed = 0;
    let mut failed = false;

    for bundle in paths.bundles() {
        println!();
        println!("[{}]", bundle.tag);
        let outcome = patcher.apply(&bundle, &kinds);
        let _ = outcome.tag;

        for note in &outcome.notes {
            println!("  - {note}");
        }
        if let Some(err) = &outcome.error {
            println!("  ! {err}");
            failed = true;
        }
        if outcome.changed {
            println!("  已写入 {}", bundle.path.display());
            changed += 1;
        }
    }

    println!();
    if failed {
        return Err("部分 bundle 未能完成补丁，见上方日志".into());
    }
    if changed == 0 {
        println!("没有文件被修改。");
    } else {
        println!("完成，共修改 {changed} 个文件。");
        println!("必须完全退出 Cursor（含所有 Cursor Private Inference 进程）后重新启动才会生效。");
    }
    Ok(())
}

fn cmd_revert(paths: &Paths) -> Result<(), String> {
    let options = PatchOptions::default();
    let backup_dir = paths.js_backup_dir();
    let patcher = Patcher::new(&options, &backup_dir);

    let mut restored = 0;
    for bundle in paths.bundles() {
        println!("[{}]", bundle.tag);
        match patcher.restore_origin(&bundle)? {
            Some(name) => {
                println!("  已从 {name} 恢复");
                restored += 1;
            }
            None => println!("  没有备份，跳过"),
        }
    }

    println!();
    if restored > 0 {
        println!("已恢复 {restored} 个文件，请完全重启 Cursor。");
    } else {
        println!("没有文件被恢复。");
    }
    Ok(())
}

fn cmd_doctor(paths: &Paths) -> Result<(), String> {
    println!("环境检查");
    println!("  sqlite      内置 winsqlite3.dll");
    println!("  state.vscdb {}", status_line(&paths.state_db_path()));
    println!("  Local State {}", status_line(&paths.local_state_path()));
    println!("  配置目录    {}", paths.config_dir.display());

    match crypto::load_master_key(&paths.local_state_path()) {
        Ok(key) => println!("  主密钥      {} 字节（DPAPI 解包成功）", key.len()),
        Err(err) => println!("  主密钥      失败: {err}"),
    }

    match providers::cursor_running() {
        Some(list) => println!("  Cursor      正在运行: {}", list.join(", ")),
        None => println!("  Cursor      未运行（可以切换）"),
    }

    println!();
    match providers::read_current(paths) {
        Ok(current) => {
            println!("当前配置");
            println!("  Base URL    {}", current.base_url.as_deref().unwrap_or("(未设置)"));
            println!("  使用自有 Key {}", current.use_open_ai_key);
            match (&current.api_key, &current.api_key_error) {
                (Some(key), _) => println!("  API Key     {}", mask(key)),
                (None, Some(err)) => println!("  API Key     不可读: {err}"),
                _ => println!("  API Key     (未设置)"),
            }
            println!("  模型数      {}", current.model_count);
            if let Some(model) = &current.composer_model {
                println!("  composer    {model}");
            }
            if let Some(base) = &current.catalog_base_url {
                println!("  模型缓存    {base}");
            } else {
                println!("  模型缓存    (无)");
            }
        }
        Err(err) => println!("读取当前配置失败: {err}"),
    }
    println!();
    Ok(())
}

fn status_line(path: &std::path::Path) -> String {
    if path.is_file() {
        format!("{}  (ok)", path.display())
    } else {
        format!("{}  (缺失)", path.display())
    }
}

// ---------------------------------------------------------------- 供应商命令

fn load_book(paths: &Paths) -> Result<ProviderBook, String> {
    ProviderBook::load(&paths.providers_file())
}

fn cmd_list(paths: &Paths) -> Result<(), String> {
    match providers::read_current(paths) {
        Ok(current) => {
            println!("当前配置");
            println!("  Base URL    {}", current.base_url.as_deref().unwrap_or("(未设置)"));
            println!("  使用自有 Key {}", current.use_open_ai_key);
            match (&current.api_key, &current.api_key_error) {
                (Some(key), _) => println!("  API Key     {}", mask(key)),
                (None, Some(err)) => println!("  API Key     不可读: {err}"),
                _ => println!("  API Key     (未设置)"),
            }
            println!("  模型数      {}", current.model_count);
            println!();
        }
        Err(err) => println!("读取当前配置失败: {err}\n"),
    }

    let book = load_book(paths)?;
    if book.providers.is_empty() {
        println!("还没有保存任何供应商，用 add 创建。");
        println!("配置文件: {}", paths.providers_file().display());
        return Ok(());
    }

    let current_base = providers::read_current(paths).ok().and_then(|c| c.base_url);
    println!("已保存的供应商（{}）", book.providers.len());
    for name in book.names() {
        let provider = &book.providers[name];
        let active = match (&current_base, &provider.base_url) {
            (Some(base), url) if base == url => "  <- 当前",
            _ => "",
        };
        println!("  {name}{active}");
        println!("    base_url  {}", provider.base_url);
        if let Some(key) = &provider.api_key {
            println!("    api_key   {}", mask(key));
        }
        if let Some(model) = &provider.default_model {
            println!("    model     {model}");
        }
        if provider.clear_catalog {
            println!("    切换时清空模型缓存");
        }
    }
    println!();
    println!("配置文件: {}", paths.providers_file().display());
    Ok(())
}

fn cmd_show(paths: &Paths, flags: &Flags) -> Result<(), String> {
    let name = flags.positional("供应商名")?;
    let book = load_book(paths)?;
    let provider = book.get(&name)?;

    println!("供应商 {name}");
    println!("  base_url  {}", provider.base_url);
    match &provider.api_key {
        Some(key) => println!("  api_key   {}", mask(key)),
        None => println!("  api_key   (未保存)"),
    }
    if let Some(model) = &provider.default_model {
        println!("  model     {model}");
    }
    if let Some(note) = &provider.note {
        println!("  备注      {note}");
    }
    Ok(())
}

fn cmd_add(paths: &Paths, flags: &Flags) -> Result<(), String> {
    let name = flags.positional("供应商名")?;
    let base_url = flags
        .value("base-url")
        .ok_or("add 需要 --base-url")?
        .to_string();

    let mut book = load_book(paths)?;
    book.providers.insert(
        name.clone(),
        Provider {
            base_url,
            api_key: flags.value("api-key").map(str::to_string),
            default_model: flags.value("default-model").map(str::to_string),
            clear_catalog: flags.has("clear-catalog"),
            note: flags.value("note").map(str::to_string),
        },
    );
    book.save(&paths.providers_file())?;

    println!("已保存供应商 {name}");
    println!("配置文件: {}", paths.providers_file().display());
    Ok(())
}

/// 一步设置：写入清单并立刻生效，不需要先 add 再 use。
///
/// 三种用法：
/// - `set --base-url <url> --api-key <key>`：新建或覆盖，名字省略时按 host 取名
/// - `set --base-url <url>`：只换端点，Key / 默认模型沿用该名字上次的值
/// - `set <name> --api-key <key>`：只换 Key，端点沿用清单里的
fn cmd_set(paths: &Paths, flags: &Flags) -> Result<(), String> {
    let mut book = load_book(paths)?;
    let explicit_name = flags.positional_opt();
    let base_url = flags.value("base-url").map(str::to_string);
    let api_key = flags.value("api-key").map(str::to_string);

    if base_url.is_none() && api_key.is_none() {
        return Err("set 至少需要 --base-url 或 --api-key 之一".into());
    }

    // 确定目标供应商名：
    //   1. 显式给了名字
    //   2. 给了 baseUrl，且清单里已有同 baseUrl 的供应商 → 复用其名字
    //   3. 给了 baseUrl → 用 host 取名
    //   4. 只有 apiKey → 必须落到当前激活项或唯一供应商
    let name = match &explicit_name {
        Some(name) => name.clone(),
        None => {
            if let Some(url) = &base_url {
                book.providers
                    .iter()
                    .find(|(_, p)| &p.base_url == url)
                    .map(|(existing, _)| existing.clone())
                    .unwrap_or_else(|| host_of(url))
            } else {
                match &book.active {
                    Some(active) => active.clone(),
                    None if book.providers.len() == 1 => {
                        book.providers.keys().next().cloned().unwrap_or_default()
                    }
                    None => {
                        return Err(
                            "只改 Key 时需要指定供应商名，或先设置一次 --base-url".into()
                        )
                    }
                }
            }
        }
    };

    let previous = book.providers.get(&name).cloned();

    // 没给 baseUrl 时必须能从上次的值取到
    let resolved_base_url = match base_url {
        Some(url) => url,
        None => previous
            .as_ref()
            .map(|p| p.base_url.clone())
            .ok_or_else(|| format!("供应商 {name} 还没有 base_url，请加 --base-url"))?,
    };

    let provider = Provider {
        base_url: resolved_base_url,
        api_key: api_key.or_else(|| previous.as_ref().and_then(|p| p.api_key.clone())),
        default_model: flags
            .value("default-model")
            .map(str::to_string)
            .or_else(|| previous.as_ref().and_then(|p| p.default_model.clone())),
        clear_catalog: if flags.has("clear-catalog") {
            true
        } else {
            previous.as_ref().map(|p| p.clear_catalog).unwrap_or(false)
        },
        note: previous.as_ref().and_then(|p| p.note.clone()),
    };

    book.providers.insert(name.clone(), provider.clone());
    book.save(&paths.providers_file())?;

    apply_provider(paths, &mut book, &name, &provider, flags)
}

fn cmd_use(paths: &Paths, flags: &Flags) -> Result<(), String> {
    let name = flags.positional("供应商名")?;

    let mut book = load_book(paths)?;
    let provider = book.get(&name)?.clone();

    apply_provider(paths, &mut book, &name, &provider, flags)
}

/// 把供应商真正写进 Cursor 配置，并记为当前激活项。
fn apply_provider(
    paths: &Paths,
    book: &mut ProviderBook,
    name: &str,
    provider: &Provider,
    flags: &Flags,
) -> Result<(), String> {
    if !flags.has("force") {
        if let Some(running) = providers::cursor_running() {
            return Err(format!(
                "Cursor 仍在运行（{}）。请完全退出后再切换，否则退出时会把旧配置写回覆盖；\
                 确认无风险可加 --force。",
                running.join(", ")
            ));
        }
    }

    let report = providers::switch_to(paths, provider, !flags.has("keep-catalog"))?;

    book.active = Some(name.to_string());
    book.save(&paths.providers_file())?;

    println!("已切换到 {name}");
    println!("  Base URL  {}", report.base_url);
    println!("  API Key   {}", if report.key_written { "已写入" } else { "保持原值" });
    if let Some(model) = &report.default_model_set {
        println!("  默认模型  {model}");
    }
    if report.catalog_cleared {
        println!("  模型缓存  已清空，下次启动会重新请求 /models");
    }
    println!("  备份      {}", report.backup.display());
    println!();
    println!("现在可以启动 Cursor。");
    Ok(())
}

/// 从 baseUrl 取一个可读的供应商名，例如 `https://api.example.com/v1` → `api.example.com`。
fn host_of(base_url: &str) -> String {
    let rest = base_url
        .split_once("://")
        .map(|(_, tail)| tail)
        .unwrap_or(base_url);
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        "default".to_string()
    } else {
        host.to_string()
    }
}

fn cmd_remove(paths: &Paths, flags: &Flags) -> Result<(), String> {
    let name = flags.positional("供应商名")?;
    let mut book = load_book(paths)?;
    if book.providers.remove(&name).is_none() {
        return Err(format!("没有名为 {name} 的供应商"));
    }
    if book.active.as_deref() == Some(name.as_str()) {
        book.active = None;
    }
    book.save(&paths.providers_file())?;
    println!("已删除供应商 {name}");
    Ok(())
}

fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "****".into();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}  (len {})", chars.len())
}

// ---------------------------------------------------------------- 参数解析

struct Flags {
    positional: Vec<String>,
    named: Vec<(String, String)>,
    switches: Vec<String>,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut positional = Vec::new();
        let mut named = Vec::new();
        let mut switches = Vec::new();

        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            if let Some(name) = arg.strip_prefix("--") {
                let name = name.to_string();
                // 布尔开关列表
                if matches!(
                    name.as_str(),
                    "keep-gate"
                        | "no-node-check"
                        | "keep-catalog"
                        | "force"
                        | "clear-catalog"
                ) {
                    switches.push(name);
                    index += 1;
                    continue;
                }
                let Some(value) = args.get(index + 1) else {
                    return Err(format!("--{name} 缺少参数值"));
                };
                named.push((name, value.clone()));
                index += 2;
            } else {
                positional.push(arg.clone());
                index += 1;
            }
        }

        Ok(Self { positional, named, switches })
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.named
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|switch| switch == name)
    }

    fn positional(&self, label: &str) -> Result<String, String> {
        self.positional
            .first()
            .cloned()
            .ok_or_else(|| format!("缺少参数: {label}"))
    }

    fn positional_opt(&self) -> Option<String> {
        self.positional.first().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_switches_and_values() {
        let args: Vec<String> = ["--patch", "protocol", "--keep-gate", "extra"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let flags = Flags::parse(&args).unwrap();
        assert_eq!(flags.value("patch"), Some("protocol"));
        assert!(flags.has("keep-gate"));
        assert_eq!(flags.positional("x").unwrap(), "extra");
    }

    #[test]
    fn reports_missing_value() {
        let args: Vec<String> = vec!["--patch".to_string()];
        assert!(Flags::parse(&args).is_err());
    }

    #[test]
    fn masks_secrets() {
        assert_eq!(mask("short"), "****");
        let masked = mask("sk-1234567890");
        assert!(masked.starts_with("sk-1"));
        assert!(masked.contains("len 13"));
    }

    #[test]
    fn derives_name_from_host() {
        assert_eq!(host_of("https://api.example.com/v1"), "api.example.com");
        assert_eq!(host_of("http://localhost:8080/v1"), "localhost:8080");
        assert_eq!(host_of("api.example.com"), "api.example.com");
        assert_eq!(host_of(""), "default");
    }
}
