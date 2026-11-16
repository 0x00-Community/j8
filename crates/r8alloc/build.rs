//! 编 C11 的分配器（stdatomic + mmap）。
fn main() {
    println!("cargo::rerun-if-changed=src/r8alloc.c");
    cc::Build::new().file("src/r8alloc.c").std("c11").warnings(true).extra_warnings(true).opt_level(2).compile("r8alloc");
}
