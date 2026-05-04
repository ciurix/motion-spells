fn main() {
    // Linker script provided by esp-hal
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}