fn main() {
    // Forward GIT_COMMIT and GIT_BRANCH env vars (set via Docker --build-arg)
    // as compile-time constants available via option_env!() in source code.
    for var in ["GIT_COMMIT", "GIT_BRANCH"] {
        if let Ok(val) = std::env::var(var) {
            println!("cargo:rustc-env={}={}", var, val);
        }
        println!("cargo:rerun-if-env-changed={}", var);
    }
}
