/// Build script for sqry-cli.
///
/// On Windows, request an 8 MB default stack for the `sqry` binary.
/// The deep clap subcommand tree (~30+ subcommands with many arguments)
/// overflows the default 1 MB stack in unoptimized (debug) builds.
fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        println!("cargo:rustc-link-arg-bin=sqry=/STACK:8388608");
    }
}
