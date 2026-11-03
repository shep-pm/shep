//! Hands the compile-time target triple to the crate as `SHEP_BUILD_TARGET`.
//!
//! Cargo sets `TARGET` for build scripts only, and the dog index needs the
//! exact triple this binary was compiled for.

fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET for every build script");
    println!("cargo:rustc-env=SHEP_BUILD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
