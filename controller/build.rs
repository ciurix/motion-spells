fn main() {
    // Linker scripts provided by cortex-m-rt and defmt
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
}