//! Embeds the icon, the application manifest and the version information in
//! the Windows executable. Does nothing for other targets.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/mulot-scan.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap().replace('\\', "/");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts = version.split(['.', '-']).map(|p| p.parse::<u16>().unwrap_or(0));
    let (major, minor, patch) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));

    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{root}/assets/icon.ico"
1 24 "{root}/assets/mulot-scan.manifest"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040C04B0"
    BEGIN
      VALUE "CompanyName", "mulot-scan"
      VALUE "FileDescription", "Mulot-scan"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "mulot-scan"
      VALUE "OriginalFilename", "mulot-scan.exe"
      VALUE "ProductName", "Mulot-scan"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x40C, 1200
  END
END
"#
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("mulot-scan.rc");
    std::fs::write(&out, rc).expect("writing resource script");
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_optional()
        .expect("compiling Windows resources (rc.exe, llvm-rc or windres required)");
}
