use std::fs;

fn main() {
    // Linker scripts provided by cortex-m-rt and defmt
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");

    // Pull the network name out of the gitignored .env at the repo root and
    // hand it to the firmware as WIFI_SSID, so the "managing <name>" screen can
    // show it without the name living in committed source. Only the SSID is
    // injected - never the passwords, which the display has no use for and which
    // must not end up baked into a binary. If .env is absent, the firmware falls
    // back to a placeholder (see option_env! in main.rs).
    println!("cargo:rerun-if-changed=../.env");
    if let Ok(contents) = fs::read_to_string("../.env") {
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(value) = line.strip_prefix("WIFI_SSID=") {
                let value = value.trim().trim_matches('"');
                println!("cargo:rustc-env=WIFI_SSID={value}");
                break;
            }
        }
    }
}
