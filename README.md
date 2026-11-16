# YumeByokForCursor

`ybc.exe` configures Cursor BYOK providers, enables the Responses protocol, and unlocks reasoning effort for all models. Windows x64 only; tested with Cursor 3.30.14.

## Usage

Quit Cursor first. For a System install, use an administrator PowerShell.

```powershell
# Save and activate a provider, applying all patches
ybc set work --base-url https://api.example.com/v1 --api-key sk-xxxx

# Change only the URL, keeping the current key
ybc set --base-url https://api.other.com/v1

# Change only the saved provider's key
ybc set work --api-key sk-yyyy

# Switch back, applying all patches for this provider
ybc use work
```

Omit the provider name to use its URL host as the name. `set` and `use` patch both bundles using the provider URL, update Cursor configuration, and clear the model cache. Add `--keep-catalog` to keep the cache. Launch Cursor afterwards.

```powershell
ybc list         # Current configuration and saved providers
ybc show work    # Show one provider
ybc remove work  # Remove a saved provider
ybc status       # Check patches and target installation
ybc revert       # Restore JS backups
ybc doctor       # Check the environment
ybc --help       # Manual patch commands and options
```

After a Cursor update, run `ybc use work` again to reapply the patches.

## Cursor installation

| Windows x64 | Download |
| --- | --- |
| User | [Installer](https://api2.cursor.sh/updates/download/golden/win32-x64-user/cursor-local/latest) |
| System | [Installer](https://api2.cursor.sh/updates/download/golden/win32-x64/cursor-local/latest) |

The tool detects the running installation, then checks User and System locations. If multiple installations are found, specify the target:

```powershell
$env:YBC_APP_DIR = 'C:\Program Files\cursor'
ybc status
```

## Configuration and backups

Stored in `%USERPROFILE%\.config\ybc`:

- `providers.yaml`: saved providers and plaintext API keys. Keys are encrypted when written to Cursor.
- `js-backups\<install-id>\`: JS backups, separated by installation. Older backups directly under `js-backups` are not restored automatically.
- `db-backups\`: database backups taken before switching providers.

Overrides: `YBC_APP_DIR` (installation), `YBC_DATA_DIR` (user data), `YBC_CONFIG_DIR` (tool configuration), and `YBC_STATE_DB` (database).

Both `cursor-agent-exec` and `cursor-local-agent-runtime` are patched. Patching checks JavaScript syntax with `node --check` when Node.js is available and restores the affected file if that check fails. A patch failure prevents provider activation.

## Build

```powershell
cargo build --release --offline
```

Output: `target\release\ybc.exe`.

## License

[BSD 3-Clause](LICENSE).
