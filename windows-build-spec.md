# Spec: Windows build with embedded icon (cross-compile from Linux + GitHub Releases)

**Project:** factorio-save-backup-manager-rust (v0.2.0, Rust 2024 + Slint 1.17)
**Date:** 2026-09-13
**Status:** Draft — approved via user interview, no code changes yet

---

## 1. Goal

Produce a Windows 64-bit `.exe` of the app that:

1. Shows the project's `icon.ico` in Windows Explorer, taskbar and title bar
   (embedded in the exe itself).
2. Carries Windows version metadata (Properties → Details tab).
3. Runs GUI-only — no console window on launch.
4. Is built **from this Linux machine** (cross-compile) and, going forward,
   automatically by **GitHub Actions** whenever a version tag like `v0.2.0`
   is pushed, attaching the exe to a GitHub Release.

---

## 2. Current state (findings)

| Fact | Detail |
|---|---|
| `build.rs` | Only compiles the Slint UI (`slint_build::compile`). No Windows resource embedding exists. |
| `icon.ico` | Single image, **244×256, non-square**, 32bpp, ~258 KB. User chose to **keep it as-is** (documented caveat in §9). |
| Source art | `factorio_chad.png` (977×1024 RGBA), `factorio_chad_bg.png` (1280×720 RGB). Not used for the icon in this iteration. |
| `src/main.rs` | No `#![windows_subsystem]` attribute → console window would appear on Windows. |
| `Cargo.toml` | Bin name `factorio-save-backup-manager-rust`, version `0.2.0`, uses `reqwest` with `native-tls` (→ Schannel on Windows, no OpenSSL cross-compile problems). |
| Git | 1 local commit (`35d7967`, `main`), remote `git@github.com:Abstem406/factorio-save-backup-manager-rust.git` (SSH). **Push failed in a previous session** — SSH key not yet added to the GitHub account. User said to **skip fixing the push** in this task. |
| CI | None. |

---

## 3. Decisions (from interview)

| Topic | Decision |
|---|---|
| Build environment | **Cross-compile from Linux** (this machine) |
| Windows target | **`x86_64-pc-windows-msvc`** (via cargo-xwin/LLVM), **not** windows-gnu |
| Fallback plan | (Answered "straight to MSVC" — no GNU-first fallback; GNU+windres documented in §8 only as emergency plan B) |
| Console window | **Hide it** — GUI-only, unconditional (release and debug) |
| Icon | **Keep current `icon.ico` as-is** (no regeneration) |
| Exe metadata | **Icon + version info** (product name, version, author, description) |
| Metadata strings | Product: **"Factorio Save Backup Manager"** · Author: **"Abstem406"** · Description: **"Backup and restore Factorio saves to Google Drive / Discord"** |
| Release trigger | **Fully automatic on tag push** (`v*`) → build + attach to GitHub Release |
| CI scope | **Build check on every push to `main`** (no release) + full release on tags. PRs not required. |
| Version policy | **Strict**: workflow fails if tag version ≠ `Cargo.toml` version |
| Asset name | **Versioned + platform**: `factorio-save-backup-manager-rust-0.2.0-win64.exe` |
| Release notes | **Minimal fixed template** (description + runtime requirements) |
| Verification | **Wine smoke test in CI** (GUI starts and stays alive under Xvfb) |
| First release | **User tags manually** — do not create/push tags as part of this task |
| Local script | **Yes** — `release-windows.sh` for manual builds |
| GitHub push (pending from last session) | **Out of scope** — user will handle; CI cannot run until the initial commit is pushed |

---

## 4. Deliverables

1. **`build.rs`** (modified) — Slint compile + Windows resource embedding.
2. **`src/main.rs`** (modified) — `#![windows_subsystem = "windows"]`.
3. **`.cargo/config.toml`** (new) — default linker/runner settings for the MSVC cross target (optional but recommended, see §6.3).
4. **`release-windows.sh`** (new) — local cross-compile + rename helper.
5. **`.github/workflows/ci.yml`** (new) — build check on push to `main`.
6. **`.github/workflows/release.yml`** (new) — tag-triggered build + smoke test + GitHub Release.
7. **`README.md`** (modified) — short "Building for Windows" section.
8. Locally installed toolchain (this machine): LLVM/clang toolchain + cargo-xwin + rustup target (§6.2).

---

## 5. Behavior spec: what the exe must contain

### 5.1 Icon resource
- The exe embeds the current repository `icon.ico` as its application icon
  (resource type `RT_GROUP_ICON`, id 1).
- Windows Explorer, taskbar, and (via the window class) the title bar pick it
  up. No runtime changes needed — Slint uses the default window icon unless
  overridden; the embedded exe icon is what Explorer/taskbar show.

### 5.2 Version info resource (VS_VERSIONINFO)
- `FileVersion` / `ProductVersion`: `0.2.0` (from `CARGO_PKG_VERSION`, four
  components: `0.2.0.0`).
- `ProductName`: `Factorio Save Backup Manager`
- `CompanyName` / author: `Abstem406`
- `FileDescription`: `Backup and restore Factorio saves to Google Drive / Discord`
- `OriginalFilename`: `factorio-save-backup-manager-rust.exe`
- Should stay in sync with `Cargo.toml` automatically (generated in build.rs
  from `CARGO_PKG_*` env vars — never hard-coded).

### 5.3 Subsystem
- `#![windows_subsystem = "windows"]` at the top of `src/main.rs`
  → PE subsystem `WINDOWS_GUI`, no console allocated on launch.
- Note: `eprintln!` log lines become invisible on Windows (acceptable; the
  in-app log pane already exists).

---

## 6. Build pipeline design

### 6.1 `build.rs` (modified)
```
fn main() {
    slint_build::compile("src/main_window.slint").unwrap();

    // Windows resource: icon + version metadata.
    // winresource no-ops for non-Windows targets, so Linux dev builds
    // are unaffected.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        // Version metadata pulled from CARGO_PKG_* (see §5.2).
        res.compile().expect("failed to compile Windows resources");
    }
}
```
- New build-dependency: `winresource = "0.1"` (maintained fork of `winres`;
  supports MSVC `rc.exe` **and** GNU `windres.exe`).
- Under cargo-xwin, resource compilation uses LLVM tools (`llvm-rc` /
  `clang-cl` / `lld-link` provided by the LLVM packages and wired up by
  cargo-xwin). If the resource compiler cannot be found the build fails with
  a clear error — that's the trigger for plan B (§8).

### 6.2 Local toolchain (one-time setup on this Linux machine)
```
sudo apt install clang lld llvm            # LLVM linker + resource compiler
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
```
- First build downloads the MSVC CRT + Windows SDK headers/libs via `xwin`
  (~1–2 GB, cached under `~/.cache/`).
- Build command: `cargo xwin build --release --target x86_64-pc-windows-msvc`
- Output: `target/x86_64-pc-windows-msvc/release/factorio-save-backup-manager-rust.exe`
- Note: the earlier interview answer "install mingw-w64" applies only to the
  fallback path (§8); the MSVC primary path needs **LLVM**, not mingw.

### 6.3 `.cargo/config.toml` (new, optional convenience)
- `[target.x86_64-pc-windows-msvc]` linker settings so plain `cargo build
  --target x86_64-pc-windows-msvc` works after cargo-xwin has set up the
  sysroot, or explicit documentation that `cargo xwin build` must be used.
- Keep minimal; cargo-xwin drives most env setup itself.

### 6.4 `release-windows.sh` (new)
Behavior:
1. Reads version from `Cargo.toml` (`cargo metadata --no-deps` or `grep`).
2. Runs `cargo xwin build --release --target x86_64-pc-windows-msvc`.
3. Copies + renames the exe to
   `dist/factorio-save-backup-manager-rust-<version>-win64.exe`.
4. Prints sha256 checksum of the artifact.
5. Fails early with a clear message if `cargo-xwin`/target/LLVM tools are
   missing (pointing to §6.2).

---

## 7. CI design (GitHub Actions)

### 7.1 `.github/workflows/ci.yml` — build check
- **Trigger:** `push` to `main`.
- **Job:** ubuntu-latest runner:
  1. checkout
  2. install Rust stable + `x86_64-pc-windows-msvc` target
  3. install clang/lld/llvm + cargo-xwin (apt + cargo-install; cache the
     xwin SDK download and cargo registry)
  4. `cargo xwin build --release --target x86_64-pc-windows-msvc`
  5. Upload exe as a workflow **artifact** (for manual testing), named with
     the short commit SHA.
- No release, no tagging. Purpose: catch cross-compile breakage before a tag.

### 7.2 `.github/workflows/release.yml` — tag release
- **Trigger:** `push` of tags matching `v*`.
- **Steps:**
  1. **Version consistency check (strict):** compare tag (`v0.2.0` →
     `0.2.0`) against version parsed from `Cargo.toml`. Any mismatch
     **fails the workflow** with a clear message ("tag v0.2.0 ≠ Cargo.toml
     0.2.0 — fix one of them and re-tag").
  2. Build with cargo-xwin (same toolchain steps as ci.yml, shared via a
     composite action or duplicated YAML — duplicate is simpler and
     acceptable at this scale).
  3. **Wine smoke test** (see §7.3).
  4. Rename artifact to
     `factorio-save-backup-manager-rust-<version>-win64.exe`.
  5. Create GitHub Release (see §7.4) and attach the exe + its sha256
     checksum file (`.sha256`).
- **Permissions:** `contents: write` (release creation).

### 7.3 Wine smoke test (in release workflow)
- Install `wine64` + `xvfb` on the runner.
- Run: `xvfb-run -a timeout 15 wine64 factorio-save-backup-manager-rust-<version>-win64.exe`
- **Pass criterion:** the process is **still alive** when the timeout kills
  it (GUI event loop running = window created successfully). If the process
  exits on its own before 15 s → fail.
- Caveat documented: software GL (llvmpipe) is used; if wine/OpenGL proves
  flaky in CI, this step may be moved behind a workflow input
  (`run_smoke_test`, default true) rather than removed.
- Local wine testing was **not** requested (CI-only verification chosen).

### 7.4 Release body — fixed minimal template
```markdown
## Factorio Save Backup Manager <version>

Portable Windows build — no installation required.

### Requirements
- Windows 10/11 64-bit
- Google Drive uploads: place your OAuth `credentials.json`
  ("Desktop app" type from Google Cloud Console) next to the exe;
  the app opens a browser login on first use (`gdrive-token.json`
  is created automatically).
- Discord notifications (optional): configure webhook / bot token
  in the app's settings.

### Notes
- The exe is not code-signed; Windows SmartScreen may show a warning —
  choose "More info" → "Run anyway".
- Config, token and credentials files are created next to the exe
  (keep it in its own folder).
```

### 7.5 Asset naming
- Exe: `factorio-save-backup-manager-rust-<version>-win64.exe`
  (e.g. `...-0.2.0-win64.exe`)
- Checksum: same name + `.sha256`

---

## 8. Risk register & plan B

| Risk | Mitigation / plan B |
|---|---|
| `llvm-rc`/winresource incompatibility under cargo-xwin (resource step fails) | Plan B: switch target to `x86_64-pc-windows-gnu` (mingw-w64 + `windres`), same build.rs works unchanged thanks to winresource's dual-toolchain support. Also acceptable: precompile `.rc` → `.res` with `llvm-rc` and pass via `cargo:rustc-link-arg`. |
| `icon.ico` is single-image 244×256 (non-square) | User chose to keep it. Windows scales it; small sizes (16×16) may look soft. Future improvement (out of scope): regenerate square multi-size ICO from `factorio_chad.png`. If the linker/resource compiler rejects the odd size, regeneration becomes mandatory — flagged in spec so it's not a surprise. |
| Wine smoke test flaky in CI (GPU/GL) | Behind a workflow input toggle; software rendering (llvmpipe) usually sufficient. |
| App hangs waiting for something under wine (OAuth, ports) | The GUI should reach its event loop without network calls at startup; smoke test only asserts "alive after 15 s", not full functionality. |
| Repo not pushed yet (SSH key pending) | CI can't run until the initial commit is pushed. Out of scope per user; listed as open item. |
| SmartScreen / unsigned exe | Documented in release template; no code-signing certificate in scope. |
| `native-tls` on Windows | Uses Schannel — no OpenSSL cross-compile issues expected. No action. |

---

## 9. Out of scope

- Regenerating `icon.ico` (user decision: keep as-is).
- Installer (Inno Setup/NSIS) — bare exe + release was chosen.
- Fixing the pending GitHub SSH push / pushing `main`.
- Creating the `v0.2.0` tag (user tags manually).
- Code signing.
- 32-bit / ARM64 builds (64-bit only chosen).

---

## 10. Acceptance criteria

1. `cargo xwin build --release --target x86_64-pc-windows-msvc` succeeds on
   this Linux machine and produces `dist/factorio-save-backup-manager-rust-0.2.0-win64.exe`
   via `release-windows.sh`.
2. The exe shows the chad icon in Windows Explorer and the Properties →
   Details tab shows product name, version 0.2.0, author and description.
3. Launching the exe on Windows shows **no console window**, only the GUI.
4. Pushing a commit to `main` runs the build-check workflow and produces a
   downloadable artifact.
5. Pushing tag `v0.2.0` (with matching Cargo.toml) produces a GitHub Release
   containing the versioned exe + checksum, after passing the wine smoke
   test. A mismatched tag fails the workflow.
6. Linux dev builds (`cargo build`) continue to work unchanged
   (winresource no-ops; slint path untouched).
