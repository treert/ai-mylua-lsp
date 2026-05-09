fn main() {
    // Capture `rustc --version` at compile time so logger.rs can embed it.
    let output = std::process::Command::new("rustc")
        .arg("--version")
        .output();
    let version = match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => "unknown".to_string(),
    };
    println!("cargo:rustc-env=RUSTC_VERSION={}", version);
}
