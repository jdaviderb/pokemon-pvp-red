//! Compile logshim.c (a C-variadic libretro log callback) into a static lib and link it.
//! Needed so the N64 core's RETRO_ENVIRONMENT_GET_LOG_INTERFACE gets a real variadic fn ptr.

use std::process::Command;

fn main() {
    let out = std::env::var("OUT_DIR").unwrap();

    let compiled = Command::new("clang")
        .args(["-O2", "-c", "logshim.c", "-o"])
        .arg(format!("{out}/logshim.o"))
        .status()
        .expect("run clang")
        .success();
    assert!(compiled, "clang failed to compile logshim.c");

    let archived = Command::new("ar")
        .args(["crs"])
        .arg(format!("{out}/liblogshim.a"))
        .arg(format!("{out}/logshim.o"))
        .status()
        .expect("run ar")
        .success();
    assert!(archived, "ar failed to archive logshim.o");

    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=logshim");
    println!("cargo:rerun-if-changed=logshim.c");
}
