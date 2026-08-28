fn main() {
    // rust-embed tracks files that existed when its macro last expanded, but Cargo also needs
    // to invalidate that expansion when a new asset is added to one of these directories.
    for directory in [
        "../../assets/fonts",
        "../../assets/icons",
        "../../assets/images",
        "../../assets/prompts",
        "../../assets/sounds",
        "../../assets/themes",
    ] {
        println!("cargo:rerun-if-changed={directory}");
    }
}
