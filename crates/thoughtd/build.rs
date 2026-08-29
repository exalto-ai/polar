use std::path::PathBuf;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest_directory = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"),
    );
    for (binary, plist) in [
        ("thoughtd", "macos/thoughtd-Info.plist"),
        ("thought-mcp-stdio", "macos/thought-mcp-stdio-Info.plist"),
    ] {
        let plist = manifest_directory.join(plist);
        println!("cargo:rerun-if-changed={}", plist.display());
        println!(
            "cargo:rustc-link-arg-bin={binary}=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
