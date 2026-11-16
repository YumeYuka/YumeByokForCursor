//! 两个扩展 bundle 的补丁：协议选择、思考等级。
//!
//! 全部基于固定锚点字符串定位，不引入正则依赖。锚点由 3.30.14 实测得到；
//! 版本变化后匹配不到会报 `Unknown`，不会破坏文件。

use std::path::Path;

use crate::paths::Bundle;

/// 协议补丁：兜底分支 `X.includes("codex")?"responses":"chat_completions"`
/// 注意：内容含 `"`，raw string 必须用 `r##"..."##`
const PROTOCOL_TAIL: &str = r##".includes("codex")?"responses":"chat_completions""##;
const PROTOCOL_MARKER: &str = r##".includes("codex")||"##;
const PROTOCOL_HOST_ANCHOR: &str = r##"(t??"").includes(""##;

/// 思考等级：档位常量 `const <spec>={param:"reasoning_effort",values:[...],defaultValue:"medium"}`
/// 前缀停在 `values:[` 之后，`values_start` 正好落在第一个值的引号上
const EFFORT_SPEC_PREFIX: &str = r##"{param:"reasoning_effort",values:["##;
/// 档位数组结束标记（`]` 及其后的固定字段）
const EFFORT_VALUES_SUFFIX: &str = r##"],defaultValue:"medium"}"##;
const EFFORT_INNER_ANCHOR: &str = "const t=function(e){";

/// 「必须声明 api_types 才给档位」的短路
const EFFORT_GATE_PRISTINE: &str = "return!0===e.extendedCapabilitiesDetected?t.effort:void 0";
const EFFORT_GATE_PATCHED: &str = "return t.effort";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolState {
    Pristine,
    Patched,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortState {
    /// 原始：只认 gpt-5
    Pristine,
    /// 已打补丁，仅对指定前缀生效
    Prefix,
    /// 已打补丁，全模型生效（可能带排除列表）
    All,
    Unknown,
}

impl ProtocolState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pristine => "pristine",
            Self::Patched => "patched",
            Self::Unknown => "unknown",
        }
    }
}

impl EffortState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pristine => "pristine",
            Self::Prefix => "prefix",
            Self::All => "all",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortScope {
    All,
    Prefix,
}

impl EffortScope {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "all" => Ok(Self::All),
            "prefix" => Ok(Self::Prefix),
            other => Err(format!("未知的 effort scope: {other}（可选 all / prefix）")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PatchOptions {
    pub host_pattern: String,
    pub scope: EffortScope,
    /// All 模式下排除的模型前缀（正则片段），留空表示不排除
    pub exclude: String,
    /// Prefix 模式下生效的模型前缀（正则片段）
    pub prefixes: String,
    pub values: Vec<String>,
    pub remove_gate: bool,
    pub node_check: bool,
}

impl Default for PatchOptions {
    fn default() -> Self {
        Self {
            host_pattern: "api.yumeyuka.moe".into(),
            scope: EffortScope::All,
            exclude: String::new(),
            prefixes: "glm|kimi|deepseek|hy|minimax|mimo|step|space-bunny".into(),
            values: vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()],
            remove_gate: true,
            node_check: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchKind {
    Protocol,
    Effort,
}

impl PatchKind {
    pub fn parse(value: &str) -> Result<Vec<Self>, String> {
        match value {
            "protocol" => Ok(vec![Self::Protocol]),
            "effort" => Ok(vec![Self::Effort]),
            "all" => Ok(vec![Self::Protocol, Self::Effort]),
            other => Err(format!("未知的补丁类型: {other}（可选 protocol / effort / all）")),
        }
    }
}

#[derive(Debug)]
pub struct BundleStatus {
    pub tag: &'static str,
    pub path: std::path::PathBuf,
    pub exists: bool,
    pub protocol: ProtocolState,
    pub effort: EffortState,
    pub sha256: Option<String>,
}

#[derive(Debug)]
pub struct BundleOutcome {
    #[allow(dead_code)]
    pub tag: &'static str,
    pub protocol: ProtocolState,
    pub effort: EffortState,
    pub changed: bool,
    pub notes: Vec<String>,
    pub error: Option<String>,
}

pub struct Patcher<'a> {
    options: &'a PatchOptions,
    backup_dir: &'a Path,
}

impl<'a> Patcher<'a> {
    pub fn new(options: &'a PatchOptions, backup_dir: &'a Path) -> Self {
        Self { options, backup_dir }
    }

    pub fn status(&self, bundle: &Bundle) -> BundleStatus {
        if !bundle.exists() {
            return BundleStatus {
                tag: bundle.tag,
                path: bundle.path.clone(),
                exists: false,
                protocol: ProtocolState::Unknown,
                effort: EffortState::Unknown,
                sha256: None,
            };
        }

        let text = std::fs::read_to_string(&bundle.path).unwrap_or_default();
        BundleStatus {
            tag: bundle.tag,
            path: bundle.path.clone(),
            exists: true,
            protocol: detect_protocol(&text),
            effort: detect_effort(&text),
            sha256: None,
        }
    }

    pub fn apply(&self, bundle: &Bundle, kinds: &[PatchKind]) -> BundleOutcome {
        let mut outcome = BundleOutcome {
            tag: bundle.tag,
            protocol: ProtocolState::Unknown,
            effort: EffortState::Unknown,
            changed: false,
            notes: Vec::new(),
            error: None,
        };

        if !bundle.exists() {
            outcome.error = Some(format!("文件不存在: {}", bundle.path.display()));
            return outcome;
        }

        let original = match std::fs::read_to_string(&bundle.path) {
            Ok(text) => text,
            Err(err) => {
                outcome.error = Some(format!("读取失败: {err}"));
                return outcome;
            }
        };

        let mut text = original.clone();

        if kinds.contains(&PatchKind::Protocol) {
            match apply_protocol(&text, &self.options.host_pattern) {
                ProtocolOutcome::Already => {
                    outcome.protocol = ProtocolState::Patched;
                    outcome.notes.push("protocol 已打过".into());
                }
                ProtocolOutcome::NoChange(state) => {
                    outcome.protocol = state;
                }
                ProtocolOutcome::Changed { text: next, note } => {
                    text = next;
                    outcome.protocol = ProtocolState::Patched;
                    outcome.notes.push(note);
                }
                ProtocolOutcome::AnchorMissing => {
                    outcome.protocol = ProtocolState::Unknown;
                    outcome.error = Some("protocol 未匹配到原始代码，版本结构可能已变化".into());
                    return outcome;
                }
            }
        } else {
            outcome.protocol = detect_protocol(&text);
        }

        if kinds.contains(&PatchKind::Effort) {
            match apply_effort(&text, self.options) {
                EffortOutcome::Already(state) => {
                    outcome.effort = state;
                    outcome.notes.push(format!("effort 已是 {} 形态", state.label()));
                }
                EffortOutcome::Changed { text: next, state, notes } => {
                    text = next;
                    outcome.effort = state;
                    outcome.notes.extend(notes);
                }
                EffortOutcome::AnchorMissing => {
                    outcome.effort = EffortState::Unknown;
                    outcome.error = Some("effort 未匹配到原始代码，版本结构可能已变化".into());
                    return outcome;
                }
            }
        } else {
            outcome.effort = detect_effort(&text);
        }

        if text == original {
            return outcome;
        }

        if let Err(err) = self.backup(bundle, &original) {
            outcome.error = Some(format!("备份失败: {err}"));
            return outcome;
        }

        if let Err(err) = std::fs::write(&bundle.path, text.as_bytes()) {
            outcome.error = Some(format!("写入失败: {err}"));
            return outcome;
        }

        if self.options.node_check {
            if let Some(err) = run_node_check(&bundle.path) {
                // 保留失败产物便于排查，同时还原正式文件
                let failed_copy = self
                    .backup_dir
                    .join(format!("main.js.{}.failed.js", bundle.tag));
                let _ = std::fs::write(&failed_copy, text.as_bytes());
                let _ = std::fs::write(&bundle.path, original.as_bytes());
                outcome.error = Some(format!(
                    "node --check 未通过，已还原: {err}（失败产物: {}）",
                    failed_copy.display()
                ));
                return outcome;
            }
            outcome.notes.push("node --check 通过".into());
        }

        outcome.changed = true;
        outcome
    }

    fn backup(&self, bundle: &Bundle, original: &str) -> Result<(), String> {
        std::fs::create_dir_all(self.backup_dir).map_err(|e| e.to_string())?;
        let name = format!("main.js.{}.{}.bak", bundle.tag, timestamp());
        std::fs::write(self.backup_dir.join(name), original.as_bytes()).map_err(|e| e.to_string())
    }

    /// 找到最早的备份并还原（备份文件名带时间戳，可以稳定排序）
    pub fn restore_origin(&self, bundle: &Bundle) -> Result<Option<String>, String> {
        let prefix = format!("main.js.{}.", bundle.tag);
        let entries = match std::fs::read_dir(self.backup_dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(None),
        };

        let mut candidates: Vec<_> = entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with(&prefix) && name.ends_with(".bak")
            })
            .collect();
        candidates.sort_by_key(|entry| entry.file_name());

        let Some(entry) = candidates.first() else {
            return Ok(None);
        };

        let content = std::fs::read(entry.path()).map_err(|e| e.to_string())?;
        std::fs::write(&bundle.path, content).map_err(|e| e.to_string())?;
        Ok(Some(entry.file_name().to_string_lossy().to_string()))
    }
}

enum ProtocolOutcome {
    Already,
    NoChange(ProtocolState),
    Changed { text: String, note: String },
    AnchorMissing,
}

fn apply_protocol(text: &str, host: &str) -> ProtocolOutcome {
    match detect_protocol(text) {
        ProtocolState::Unknown => return ProtocolOutcome::AnchorMissing,
        ProtocolState::Patched => {
            // host 换了要重写；否则原样返回
            return match retarget_host(text, host) {
                Some(next) => ProtocolOutcome::Changed { text: next, note: "protocol 已更新 host".into() },
                None => ProtocolOutcome::Already,
            };
        }
        ProtocolState::Pristine => {}
    }

    let tail_at = match text.find(PROTOCOL_TAIL) {
        Some(at) => at,
        None => return ProtocolOutcome::AnchorMissing,
    };
    let ident_start = match identifier_start(text, tail_at) {
        Some(start) => start,
        None => return ProtocolOutcome::NoChange(ProtocolState::Unknown),
    };

    let ident = &text[ident_start..tail_at];
    let from = &text[ident_start..tail_at + PROTOCOL_TAIL.len()];
    let to = format!(r#"{ident}.includes("codex")||(t??"").includes("{host}")?"responses":"chat_completions""#);

    ProtocolOutcome::Changed { text: text.replacen(from, &to, 1), note: "protocol 已修补".into() }
}

fn retarget_host(text: &str, host: &str) -> Option<String> {
    let anchor_at = text.find(PROTOCOL_HOST_ANCHOR)?;
    let value_start = anchor_at + PROTOCOL_HOST_ANCHOR.len();
    let value_end = value_start + text[value_start..].find('"')?;
    let current = &text[value_start..value_end];
    if current == host {
        return None;
    }
    let from = format!(r#"(t??"").includes("{current}")"#);
    let to = format!(r#"(t??"").includes("{host}")"#);
    Some(text.replacen(&from, &to, 1))
}

fn identifier_start(text: &str, end: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = end;
    while index > 0 {
        let ch = bytes[index - 1];
        if ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'$' || ch == b'.' {
            index -= 1;
        } else {
            break;
        }
    }
    if index == end {
        None
    } else {
        Some(index)
    }
}

/// 解析出来的 effort 函数布局
struct EffortLayout {
    matcher: String,
    spec: String,
    /// 内层归一化函数原文
    inner: String,
    /// 内层函数结束位置（匹配函数体的真正起点）
    inner_end: usize,
    /// 匹配函数体的完整内容（`<inner_end>` 到函数结尾 `}` 之前）
    body: String,
    /// 函数体结束位置（指向结尾 `}`）
    body_end: usize,
}

/// 提取 effort 函数布局。`body` 包含内层函数之后到 `return <spec>`（含）的整段。
///
/// 关键点：`function <matcher>(e){` 在 bundle 里可能有十几处重名（压缩后的短名），
/// 必须从档位常量之后开始定位，不能从头 `find`。
fn parse_effort(text: &str) -> Option<EffortLayout> {
    let spec_prefix_at = text.find(EFFORT_SPEC_PREFIX)?;
    let const_at = text[..spec_prefix_at].rfind("const ")?;
    let spec_start = const_at + "const ".len();
    let spec_end = spec_start + text[spec_start..].find('=')?;
    let spec = text[spec_start..spec_end].trim().to_string();
    if spec.is_empty() || !spec.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return None;
    }

    // `const <spec>=...};function <matcher>(e){`
    let fn_keyword = "};function ";
    let body_start = spec_prefix_at + text[spec_prefix_at..].find(fn_keyword)? + fn_keyword.len();
    let matcher_end = body_start + text[body_start..].find('(')?;
    let matcher = text[body_start..matcher_end].trim().to_string();
    if matcher.is_empty() {
        return None;
    }

    // matcher 的签名的花括号起点
    let brace_at = body_start + text[body_start..].find("){")? + "){".len();

    let inner_start = brace_at + text[brace_at..].find(EFFORT_INNER_ANCHOR)?;
    let inner_end = inner_start + text[inner_start..].find("}(e);")? + "}(e);".len();
    let inner = text[inner_start..inner_end].to_string();

    let spec_ref = format!("return {spec}");
    let return_at = inner_end + text[inner_end..].find(&spec_ref)?;
    let body_end = return_at + spec_ref.len();
    let body = text[inner_end..body_end].to_string();

    Some(EffortLayout { matcher, spec, inner, inner_end, body, body_end })
}

enum EffortOutcome {
    Already(EffortState),
    Changed { text: String, state: EffortState, notes: Vec<String> },
    AnchorMissing,
}

fn apply_effort(text: &str, options: &PatchOptions) -> EffortOutcome {
    let layout = match parse_effort(text) {
        Some(layout) => layout,
        None => return EffortOutcome::AnchorMissing,
    };

    let desired_state = match options.scope {
        EffortScope::All => EffortState::All,
        EffortScope::Prefix => EffortState::Prefix,
    };
    let desired_body = build_body(&layout.inner, &layout.spec, options);

    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // 1) 重写匹配函数（只替换内层归一化函数之后的部分，保留 function 名与内层逻辑）
    if layout.body != desired_body {
        edits.push((layout.inner_end, layout.body_end, desired_body));
        notes.push(format!("匹配函数改为 {} 形态", desired_state.label()));
    }

    // 2) 档位常量
    if let Some((start, end, replacement)) = rewrite_values(text, &layout.spec, &options.values) {
        edits.push((start, end, replacement));
        notes.push(format!("档位改为 [{}]", options.values.join(",")));
    }

    // 3) api_types 前置条件
    if options.remove_gate {
        if let Some(at) = text.find(EFFORT_GATE_PRISTINE) {
            edits.push((at, at + EFFORT_GATE_PRISTINE.len(), EFFORT_GATE_PATCHED.to_string()));
            notes.push("移除 api_types 前置条件".into());
        }
    }

    // 4) 调用点：return <matcher>(e.id)}(e) → return <spec>}(e)
    let invocation = format!("return {}(e.id)}}(e)", layout.matcher);
    if let Some(at) = text.find(&invocation) {
        edits.push((at, at + invocation.len(), format!("return {}}}(e)", layout.spec)));
        notes.push("调用点改为直接返回档位规格".into());
    }

    if edits.is_empty() {
        return EffortOutcome::Already(desired_state);
    }

    // 从后往前应用，保证前面的偏移不失效
    edits.sort_by_key(|(start, _, _)| *start);
    let mut out = text.to_string();
    for (start, end, replacement) in edits.into_iter().rev() {
        out.replace_range(start..end, &replacement);
    }

    EffortOutcome::Changed { text: out, state: desired_state, notes }
}

/// 只生成函数体的后半段（`<inner>` 之后的部分），与 `EffortLayout::body` 语义一致。
fn build_body(inner: &str, spec: &str, options: &PatchOptions) -> String {
    let _ = inner;
    match options.scope {
        EffortScope::All => {
            if options.exclude.trim().is_empty() {
                format!("return {spec}")
            } else {
                format!("if(!/^(?:{})/u.test(t))return {spec}", options.exclude)
            }
        }
        EffortScope::Prefix => format!(
            r#"if(/^gpt-5(?:\.\d+)?$/u.test(t)||/^(?:{})/u.test(t))return {spec}"#,
            options.prefixes
        ),
    }
}

fn classify_body(body: &str) -> EffortState {
    if body.contains("||/^(?:") {
        return EffortState::Prefix;
    }
    if body.contains("if(!/^(?:") {
        return EffortState::All;
    }
    if body.starts_with("return ") {
        return EffortState::All;
    }
    if body.contains("gpt-5") {
        return EffortState::Pristine;
    }
    EffortState::All
}

/// 找到档位数组，返回 (起始, 结束, 替换内容)；已相同则 `None`
///
/// 注意：bundle 里可能有多个同名短变量（`const w=` 也用于无关工具函数），
/// 所以必须用 `{param:"reasoning_effort"` 这个特征串定位，而不是盲找 `const w=`。
fn rewrite_values(text: &str, spec: &str, values: &[String]) -> Option<(usize, usize, String)> {
    let expect = format!("const {spec}=");
    let mut search_from = 0;

    // 逐个候选核对：特征串紧跟在 `const <spec>=` 之后才算命中
    let spec_prefix_at = loop {
        let found = search_from + text[search_from..].find(EFFORT_SPEC_PREFIX)?;
        if found >= expect.len() && &text[found - expect.len()..found] == expect.as_str() {
            break found;
        }
        search_from = found + 1;
    };

    let values_start = spec_prefix_at + EFFORT_SPEC_PREFIX.len();
    let values_end = values_start + text[values_start..].find(EFFORT_VALUES_SUFFIX)?;

    let quoted: Vec<String> = values.iter().map(|v| format!("\"{v}\"")).collect();
    let replacement = quoted.join(",");

    if &text[values_start..values_end] == replacement {
        return None;
    }

    Some((values_start, values_end, replacement))
}
pub fn detect_protocol(text: &str) -> ProtocolState {
    if text.contains(PROTOCOL_TAIL) {
        ProtocolState::Pristine
    } else if text.contains(PROTOCOL_MARKER) {
        ProtocolState::Patched
    } else {
        ProtocolState::Unknown
    }
}

pub fn detect_effort(text: &str) -> EffortState {
    match parse_effort(text) {
        Some(layout) => classify_body(&layout.body),
        None => EffortState::Unknown,
    }
}

fn run_node_check(path: &Path) -> Option<String> {
    let output = std::process::Command::new("node")
        .arg("--check")
        .arg(path)
        .output()
        .ok()?;

    if output.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail: Vec<&str> = stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(3)
        .collect();
    Some(detail.join(" | "))
}

fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROTOCOL_SAMPLE: &str = r#"return n.includes("codex")?"responses":"chat_completions"}(e,t?.baseUrl)"#;

    fn bundle_text(body: &str) -> String {
        format!(
            r#"const w={{param:"reasoning_effort",values:["low","medium","high","xhigh"],defaultValue:"medium"}};function v(e){{const t=function(e){{return e}}(e);{body}return w}}const _={{x:1}};use(return v(e.id)}}(e));"#
        )
    }

    #[test]
    fn detects_pristine_protocol() {
        assert_eq!(detect_protocol(PROTOCOL_SAMPLE), ProtocolState::Pristine);
    }

    #[test]
    fn patches_protocol() {
        match apply_protocol(PROTOCOL_SAMPLE, "api.example.com") {
            ProtocolOutcome::Changed { text, .. } => {
                assert!(text.contains(r#"(t??"").includes("api.example.com")"#));
                assert!(!text.contains(r#""codex"?"responses""#));
                assert_eq!(detect_protocol(&text), ProtocolState::Patched);
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn retargets_host() {
        let patched = r#"return n.includes("codex")||(t??"").includes("old.host")?"responses":"chat_completions""#;
        assert_eq!(detect_protocol(patched), ProtocolState::Patched);
        match apply_protocol(patched, "new.host") {
            ProtocolOutcome::Changed { text, .. } => {
                assert!(text.contains("new.host"));
                assert!(!text.contains("old.host"));
            }
            _ => panic!("expected retarget"),
        }
    }

    #[test]
    fn protocol_idempotent() {
        let patched = r#"return n.includes("codex")||(t??"").includes("a.host")?"responses":"chat_completions""#;
        assert!(matches!(apply_protocol(patched, "a.host"), ProtocolOutcome::Already));
    }

    #[test]
    fn detects_pristine_effort() {
        let text = bundle_text(r#"if(/^gpt-5(?:\.\d+)?$/u.test(t))return w"#);
        assert_eq!(detect_effort(&text), EffortState::Pristine);
    }

    #[test]
    fn applies_all_scope() {
        let text = bundle_text(r#"if(/^gpt-5(?:\.\d+)?$/u.test(t))return w"#);
        match apply_effort(&text, &PatchOptions::default()) {
            EffortOutcome::Changed { text: next, state, .. } => {
                assert_eq!(state, EffortState::All);
                assert_eq!(detect_effort(&next), EffortState::All);
                assert!(!next.contains("gpt-5"));
                assert!(next.contains("return w"));
                assert!(next.contains(r#"use(return w}(e))"#));
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn applies_prefix_scope() {
        let text = bundle_text(r#"if(/^gpt-5(?:\.\d+)?$/u.test(t))return w"#);
        let mut options = PatchOptions::default();
        options.scope = EffortScope::Prefix;
        match apply_effort(&text, &options) {
            EffortOutcome::Changed { text: next, state, .. } => {
                assert_eq!(state, EffortState::Prefix);
                assert_eq!(detect_effort(&next), EffortState::Prefix);
                assert!(next.contains("||/^(?:"));
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn applies_exclude_list() {
        let text = bundle_text(r#"if(/^gpt-5(?:\.\d+)?$/u.test(t))return w"#);
        let mut options = PatchOptions::default();
        options.exclude = "gemini|grok".into();
        match apply_effort(&text, &options) {
            EffortOutcome::Changed { text: next, .. } => {
                assert!(next.contains("if(!/^(?:gemini|grok)/u.test(t))return w"));
                assert_eq!(detect_effort(&next), EffortState::All);
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn rewrites_values() {
        let text = bundle_text(r#"if(/^gpt-5(?:\.\d+)?$/u.test(t))return w"#);
        let layout = parse_effort(&text).expect("parse");
        println!(
            "matcher={} spec={} inner={} body={:?} inner_end={} body_end={}",
            layout.matcher, layout.spec, layout.inner, layout.body, layout.inner_end, layout.body_end
        );
        let mut options = PatchOptions::default();
        options.values = vec!["none".into(), "high".into()];
        match apply_effort(&text, &options) {
            EffortOutcome::Changed { text: next, .. } => {
                println!("ACTUAL: {next}");
                assert!(next.contains(r#"values:["none","high"],defaultValue:"medium""#));
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn removes_gate() {
        let text = format!(
            "{}function j(e){{{};}}",
            bundle_text(r#"if(/^gpt-5(?:\.\d+)?$/u.test(t))return w"#),
            EFFORT_GATE_PRISTINE
        );
        match apply_effort(&text, &PatchOptions::default()) {
            EffortOutcome::Changed { text: next, .. } => {
                assert!(next.contains(EFFORT_GATE_PATCHED));
                assert!(!next.contains("extendedCapabilitiesDetected?t.effort"));
            }
            _ => panic!("expected change"),
        }
    }

    #[test]
    fn idempotent_when_already_all() {
        let text = bundle_text(r#"if(/^gpt-5(?:\.\d+)?$/u.test(t))return w"#);
        let options = PatchOptions::default();
        let EffortOutcome::Changed { text: once, .. } = apply_effort(&text, &options) else {
            panic!("first apply must change")
        };
        assert!(matches!(apply_effort(&once, &options), EffortOutcome::Already(EffortState::All)));
    }

    #[test]
    fn unknown_without_anchor() {
        assert_eq!(detect_effort("nothing"), EffortState::Unknown);
        assert_eq!(detect_protocol("nothing"), ProtocolState::Unknown);
        assert!(matches!(apply_effort("nothing", &PatchOptions::default()), EffortOutcome::AnchorMissing));
    }

    /// 用 Cursor 3.30.14 的真实代码片段校验解析器
    #[test]
    fn parses_real_cursor_snippet() {
        const REAL: &str = concat!(
            r##"const w={param:"reasoning_effort",values:["low","medium","high","xhigh"],defaultValue:"medium"};"##,
            r##"function v(e){const t=function(e){let t=e.trim().toLowerCase();const r=t.lastIndexOf("/");-1!==r&&(t=t.slice(r+1));const n=t.indexOf("@");return-1!==n&&(t=t.slice(0,n)),t}(e);if(/^gpt-5(?:\.\d+)?$/u.test(t))return w}"##,
            r##"const _={param:"output_config.effort",values:["low","medium","high","max"],defaultValue:"high"};"##,
            r##"function j(e,t,r){const n=function(e){const t=I(e.id);if(void 0!==t?.effort){return!0===e.extendedCapabilitiesDetected?t.effort:void 0}return v(e.id)}(e);return n}"##
        );

        assert_eq!(detect_effort(REAL), EffortState::Pristine);

        let options = PatchOptions::default();
        let EffortOutcome::Changed { text, state, .. } = apply_effort(REAL, &options) else {
            panic!("apply must change real snippet");
        };
        assert_eq!(state, EffortState::All);
        assert_eq!(detect_effort(&text), EffortState::All);

        // 内层归一化逻辑必须原样保留
        assert!(text.contains(r#"let t=e.trim().toLowerCase();"#));
        // 档位常量原样（默认值就是目标档位）
        assert!(text.contains(r##"values:["low","medium","high","xhigh"],defaultValue:"medium""##));
        // 交叉匹配函数必须保留（`_` 是 anthropic 规格，不能动）
        assert!(text.contains(r##"const _={param:"output_config.effort""##));
        // 前置条件已移除
        assert!(text.contains("return t.effort"));
        // 调用点改为直接返回规格
        assert!(text.contains("return w}(e)"));
        assert!(!text.contains("v(e.id)}(e)"));

        // 幂等
        assert!(matches!(apply_effort(&text, &options), EffortOutcome::Already(EffortState::All)));
    }

    /// 真实 bundle 里存在同名短变量（`const w=async(...)` 等），不能误命中
    #[test]
    fn ignores_unrelated_same_name_const() {
        const REAL: &str = concat!(
            r##"const w=async({minDelay:e,maxDelay:t})=>{const c={};return c};"##,
            r##"const w={param:"reasoning_effort",values:["low","medium","high","xhigh"],defaultValue:"medium"};"##,
            r##"function v(e){const t=function(e){return e.toLowerCase()}(e);if(/^gpt-5(?:\.\d+)?$/u.test(t))return w}"##
        );

        assert_eq!(detect_effort(REAL), EffortState::Pristine);

        let mut options = PatchOptions::default();
        options.values = vec!["high".into()];
        let EffortOutcome::Changed { text, .. } = apply_effort(REAL, &options) else {
            panic!("apply must change");
        };

        // 无关的 async 函数体必须原样保留
        assert!(text.contains("const w=async({minDelay:e,maxDelay:t})=>{const c={};return c};"));
        // 档位常量被正确改写
        assert!(text.contains(r##"const w={param:"reasoning_effort",values:["high"],defaultValue:"medium"}"##));
        // 长度变化只来自档位字符串，不应该丢内容
        assert!(text.len() < REAL.len() + 32);
        assert!(text.len() > REAL.len() - 64);
    }

    /// 真实片段里的协议兜底分支
    #[test]
    fn parses_real_protocol_snippet() {
        const REAL: &str = concat!(
            r##"if(function(e){if(void 0===e)return!1;try{return"api.openai.com"===new URL(e).hostname.toLowerCase()}catch{return!1}}(t)&&n.startsWith("gpt-5"))return"responses";"##,
            r##"return n.includes("codex")?"responses":"chat_completions"}(e,t?.baseUrl)"##
        );

        assert_eq!(detect_protocol(REAL), ProtocolState::Pristine);
        let ProtocolOutcome::Changed { text, .. } = apply_protocol(REAL, "api.yumeyuka.moe") else {
            panic!("apply must change real snippet");
        };
        assert_eq!(detect_protocol(&text), ProtocolState::Patched);
        assert!(text.contains(r##"(t??"").includes("api.yumeyuka.moe")"##));
        // 前面的 api.openai.com 分支必须保留
        assert!(text.contains(r##""api.openai.com"===new URL(e).hostname.toLowerCase()"##));
    }
}
