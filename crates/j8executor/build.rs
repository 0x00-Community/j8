use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(tail_calls)");
    println!("cargo::rerun-if-env-changed=RUSTC");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| String::from("rustc"));
    let version = Command::new(rustc).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    if version.contains("-nightly") {
        println!("cargo::rustc-cfg=tail_calls");
    }
}
