fn main() {
    // Emit git commit hash and branch at build time via the `built` crate.
    built::write_built_file().expect("failed to write build metadata");
}
