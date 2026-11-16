# YumeByokForCursor

`ybc.exe`, a CLI that patches Cursor's custom OpenAI-compatible endpoint (BYOK) and switches providers. Verified against Cursor 3.30.14 only.

The patches route custom-endpoint requests to `/v1/responses` and make reasoning effort available for every model (by default it only covers the families Cursor ships with). Cursor still fetches `/models` on its own.

## Download Cursor

User installer (per-user install, no admin rights):

| Platform | Link |
| --- | --- |
| Windows x64 | https://api2.cursor.sh/updates/download/golden/win32-x64-user/cursor-local/latest |
| Windows ARM64 | https://api2.cursor.sh/updates/download/golden/win32-arm64-user/cursor-local/latest |
| macOS Apple Silicon | https://api2.cursor.sh/updates/download/golden/darwin-arm64/cursor-local/latest |
| macOS Intel | https://api2.cursor.sh/updates/download/golden/darwin-x64/cursor-local/latest |
| macOS Universal | https://api2.cursor.sh/updates/download/golden/darwin-universal/cursor-local/latest |

System installer (installs to Program Files, needs admin rights):

| Platform | Link |
| --- | --- |
| Windows x64 | https://api2.cursor.sh/updates/download/golden/win32-x64/cursor-local/latest |

## Build

```powershell
cargo build --release --offline
```

Output is `target\release\ybc.exe`, about 500 KB, no network needed.

## 1. Switch providers (the everyday command)

One command sets it and takes effect immediately — no separate save-then-switch step:

```powershell
ybc set --base-url https://api.example.com/v1 --api-key sk-xxxx
```

Without a name, the provider is named after the host (the above becomes `api.example.com`). To name it yourself, put the name first:

```powershell
ybc set work --base-url https://api.example.com/v1 --api-key sk-xxxx
```

Changing just one field keeps the other from last time:

```powershell
ybc set --base-url https://api.other.com/v1    # endpoint only, key unchanged
ybc set --api-key sk-yyyy                      # key only, endpoint unchanged
ybc set work --api-key sk-yyyy                 # pick which one
```

Inspect and switch:

```powershell
ybc list           # current config + all providers
ybc show work      # one provider
ybc use work       # switch to a saved provider
ybc remove work    # delete
```

`ybc use` and `ybc set` check for running Cursor processes first and refuse if any are found. Restart Cursor afterwards.

The model cache is cleared by default so Cursor refetches `/models` on next launch. Add `--keep-catalog` to keep it.

## 2. Patch (run once after installing or updating Cursor)

```powershell
ybc apply      # apply all patches
ybc status     # show state
ybc revert     # restore
ybc doctor     # environment check
```

Apply one kind, or adjust parameters:

```powershell
ybc apply --patch protocol
ybc apply --patch effort
ybc apply --host-pattern api.example.com
ybc apply --effort-scope all
ybc apply --effort-scope prefix
ybc apply --effort-exclude "gemini|grok"
ybc apply --effort-values "none,low,medium,high,xhigh,max"
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--host-pattern` | `api.yumeyuka.moe` | baseUrl fragment the protocol patch matches |
| `--effort-scope` | `all` | `all` for every model; `prefix` for listed prefixes |
| `--effort-exclude` | empty | model prefixes to skip in `all` mode |
| `--effort-prefixes` | `glm\|kimi\|deepseek\|hy\|minimax\|mimo\|step\|space-bunny` | prefixes that apply in `prefix` mode |
| `--effort-values` | `low,medium,high,xhigh` | reasoning effort tiers |
| `--keep-gate` | off | keep the `api_types` precondition |
| `--no-node-check` | off | skip `node --check` |

Fully quit Cursor after editing the JS.

## Config location

`~\.config\ybc\`

| File | Contents |
| --- | --- |
| `providers.yaml` | Provider list |
| `js-backups\` | JS captured before patching |
| `db-backups\` | Database copies taken before switching |

```yaml
active: work
providers:
  work:
    base_url: https://api.example.com/v1
    api_key: sk-xxxx
    default_model: glm-5.3
    clear_catalog: true
```

Keys are stored in plaintext. They are re-encrypted into Cursor's v10 format (DPAPI + AES-256-GCM) when written to `state.vscdb`, so the plaintext lives only in this file.

## Lines the patch changes

Both bundles get the same edits; only the minified variable names differ:

- `resources\app\extensions\cursor-agent-exec\dist\main.js`
- `resources\app\extensions\cursor-local-agent-runtime\dist\main.js`

Diffs below come from `cursor-agent-exec` (variables `r`/`w`; the other uses `n`/`y`). The files are minified single lines; they are wrapped here for reading only.

Protocol fallback branch:

```diff
-return r.includes("codex")?"responses":"chat_completions"
+return r.includes("codex")||(t??"").includes("api.yumeyuka.moe")?"responses":"chat_completions"
```

Reasoning-effort matcher (`--effort-scope all`, the default):

```diff
-if(/^gpt-5(?:\.\d+)?$/u.test(t))return w
+return w
```

With `--effort-scope prefix`:

```diff
+if(/^gpt-5(?:\.\d+)?$/u.test(t)||/^(?:glm|kimi|deepseek|hy|minimax|mimo|step|space-bunny)/u.test(t))return w
```

With `--effort-exclude "gemini|grok"`:

```diff
+if(!/^(?:gemini|grok)/u.test(t))return w
```

Removing the `api_types` precondition:

```diff
-return!0===e.extendedCapabilitiesDetected?t.effort:void 0
+return t.effort
```

Call site:

```diff
-return v(e.id)}(e)
+return w}(e)
```

Tiers change only with `--effort-values`:

```diff
-values:["low","medium","high","xhigh"]
+values:["none","low","medium","high","xhigh","max"]
```

Untouched: `output_config.effort` (Anthropic), `minimal`-based `reasoning_effort` (Grok), `product.json`, the workbench bundle, and the `/models` probing logic.

`product.json` checksums do not cover `extensions/*`, so editing these two bundles does not trip an integrity check.

## Environment overrides

| Variable | Default |
| --- | --- |
| `YBC_APP_DIR` | `%LOCALAPPDATA%\Programs\cursor` |
| `YBC_DATA_DIR` | `%APPDATA%\Cursor Private Inference` |
| `YBC_CONFIG_DIR` | `%USERPROFILE%\.config\ybc` |
| `YBC_STATE_DB` | `<DATA_DIR>\User\globalStorage\state.vscdb` |

With `XDG_CONFIG_HOME` set, the config directory becomes `$XDG_CONFIG_HOME\ybc`.

## Caveats

- A Cursor update overwrites both bundles; run `apply` again afterwards.
- `status` reporting `unknown` means the anchor no longer matches; nothing is written in that case.
- `apply` runs `node --check` after writing. On failure the real file is restored and the bad output is kept at `js-backups\main.js.<tag>.failed.js`.
