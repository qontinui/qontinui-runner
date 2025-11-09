# Fix Tauri Version Mismatch

**Error:**
```
tauri (v2.8.5) : @tauri-apps/api (v2.9.0)
```

**Cause:** Rust crate and NPM package must be on same minor version.

---

## Quick Fix (Option 1: Update Rust - Recommended)

**In Windows PowerShell:**

```powershell
cd C:\Users\Joshua\Documents\qontinui_parent_directory\qontinui-runner\src-tauri

# Edit Cargo.toml
notepad Cargo.toml
```

**Find this line:**
```toml
tauri = { version = "2.8", features = [...] }
```

**Change to:**
```toml
tauri = { version = "2", features = [...] }
```

**Also update the plugins:**
```toml
tauri-plugin-opener = "2"
tauri-plugin-dialog = "2"
tauri-plugin-updater = "2"
```

**And tauri-build:**
```toml
tauri-build = { version = "2", features = [] }
```

**Why "2" instead of "2.9"?**
The plugins are at v2.4.x while core is at v2.9.x. Using "2" tells Cargo to accept any compatible 2.x version.

**Save and close, then:**

```powershell
# Go back to root
cd ..

# Try build again
npm run tauri build
```

---

## Alternative Fix (Option 2: Downgrade NPM)

```powershell
cd C:\Users\Joshua\Documents\qontinui_parent_directory\qontinui-runner

# Downgrade @tauri-apps/api to match Rust version
npm install @tauri-apps/api@~2.8.0

# Try build again
npm run tauri build
```

---

## Which to Choose?

**Use Option 1** (update Rust to 2.9) because:
- Gets you latest features
- Matches your current NPM package
- Less likely to break things

**Use Option 2** (downgrade NPM to 2.8) if:
- Option 1 fails
- You want to stay conservative

---

## After Fixing

Run build again:

```powershell
npm run tauri build
```

Should work now!
