# Quick Start: Publish Runner v0.1.0

**Time:** ~30 minutes | **Tool:** Windows PowerShell (NOT WSL!)

---

## Step 1: Build (10 min)

```powershell
cd D:\qontinui_parent_directory\qontinui-runner
npm run tauri build
```

**Output:** `src-tauri\target\release\bundle\msi\Qontinui Runner_0.1.0_x64_en-US.msi`

---

## Step 2: Test (2 min)

```powershell
cd src-tauri\target\release\bundle\msi
.\Qontinui` Runner_0.1.0_x64_en-US.msi
```

Verify app launches.

---

## Step 3: Checksum (1 min)

```powershell
certutil -hashfile "Qontinui Runner_0.1.0_x64_en-US.msi" SHA256 > checksums.txt
```

---

## Step 4: GitHub Release (10 min)

1. Go to: https://github.com/qontinui/qontinui-runner/releases
2. Click "Draft a new release"
3. Tag: `v0.1.0`
4. Title: `Qontinui Runner v0.1.0`
5. Copy description from: `release-notes-v0.1.0.md` (in this directory)
6. Upload: `Qontinui Runner_0.1.0_x64_en-US.msi` and `checksums.txt`
7. Check "This is a pre-release"
8. Click "Publish release"

---

## Step 5: Update README (5 min)

Edit `README.md`, find the Installation section, add:

```markdown
### Download Pre-built Binaries

**Latest Release: [v0.1.0](https://github.com/qontinui/qontinui-runner/releases/tag/v0.1.0)**

**Windows:**
- [Download MSI Installer](https://github.com/qontinui/qontinui-runner/releases/download/v0.1.0/Qontinui.Runner_0.1.0_x64_en-US.msi)

**macOS / Linux:**
- Build from source (instructions below)
```

Then commit:

```powershell
git add README.md
git commit -m "Add download links for v0.1.0 release"
git push origin main
```

---

## Done! ✅

Your runner is published and ready to mention on HN.

**If you run into issues:** See `QONTINUI-RUNNER-BUILD-GUIDE.md` for troubleshooting.
