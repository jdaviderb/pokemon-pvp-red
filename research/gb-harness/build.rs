fn main() {
    // Compile the C variadic log shim and link it statically.
    let out = std::env::var("OUT_DIR").unwrap();
    let status = std::process::Command::new("clang")
        .args(["-O2", "-arch", "arm64", "-c", "logshim.c", "-o"])
        .arg(format!("{}/logshim.o", out))
        .status()
        .unwrap();
    assert!(status.success());
    let lib = std::process::Command::new("ar")
        .args(["crs"])
        .arg(format!("{}/liblogshim.a", out))
        .arg(format!("{}/logshim.o", out))
        .status()
        .unwrap();
    assert!(lib.success());
    println!("cargo:rustc-link-search=native={}", out);
    println!("cargo:rustc-link-lib=static=logshim");
    println!("cargo:rerun-if-changed=logshim.c");
}
