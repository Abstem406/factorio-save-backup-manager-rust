fn main() {
    slint_build::compile("src/main_window.slint").unwrap();

    // Windows-only: embed application icon + version metadata into the exe.
    // This block no-ops for non-Windows targets (Linux dev builds unaffected).
    let target_is_windows =
        std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if !target_is_windows {
        return;
    }

    let version = env!("CARGO_PKG_VERSION");
    let exe_name = format!("{}.exe", env!("CARGO_PKG_NAME"));

    let mut res = winresource::WindowsResource::new();
    res.set_icon("icon.ico");
    res.set("ProductName", "Factorio Save Backup Manager");
    res.set("CompanyName", "Abstem406");
    res.set(
        "FileDescription",
        "Backup and restore Factorio saves to Google Drive / Discord",
    );
    res.set("FileVersion", version);
    res.set("ProductVersion", version);
    res.set("OriginalFilename", &exe_name);

    let host_is_windows = std::env::consts::OS == "windows";
    if host_is_windows {
        // Native Windows build: winresource locates rc.exe from the
        // Windows SDK registry keys by itself.
        res.compile().expect("failed to compile Windows resources");
        return;
    }

    // Cross-compile from Linux (e.g. via cargo-xwin): winresource cannot
    // find rc.exe there, so we generate the .rc ourselves, compile it with
    // LLVM's llvm-rc and hand the resulting .res straight to lld-link.
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_dir = std::path::Path::new(&out_dir);
    let rc_file = out_dir.join("resource.rc");
    res.write_resource_file(&rc_file)
        .expect("failed to generate Windows resource file");

    let res_file = out_dir.join("resource.res");
    let llvm_rc = find_llvm_rc().unwrap_or_else(|| {
        panic!(
            "llvm-rc not found on PATH. Install LLVM (e.g. `sudo apt install \
             llvm`) so the Windows resource (icon/version info) can be compiled."
        )
    });
    let status = std::process::Command::new(&llvm_rc)
        .args(["/NOLOGO", "/FO"])
        .arg(&res_file)
        .arg(&rc_file)
        .status()
        .expect("failed to run llvm-rc");
    if !status.success() {
        panic!("{llvm_rc:?} failed with {status}");
    }

    // Link the compiled resource into the binary.
    println!("cargo:rustc-link-arg-bins={}", res_file.display());
}

/// Find an llvm-rc binary on PATH (handles version-suffixed names such as
/// `llvm-rc-18` used by Debian/Ubuntu packages).
fn find_llvm_rc() -> Option<String> {
    const SUFFIXES: [&str; 10] = ["", "-19", "-18", "-17", "-16", "-15", "-14",
        "-20", "-21", "-22"];
    for suffix in SUFFIXES {
        let name = format!("llvm-rc{suffix}");
        if std::process::Command::new(&name)
            .arg("/?")
            .output()
            .map(|o| o.status.success() || !o.stderr.is_empty())
            .unwrap_or(false)
        {
            return Some(name);
        }
    }
    None
}
