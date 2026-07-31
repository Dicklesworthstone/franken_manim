use std::env;
use std::path::Path;

const PROFILE_ENV: &str = "FMN_CONFORMANCE_CARGO_PROFILE";

fn selected_profile(out_dir: &Path) -> Result<&str, String> {
    let directory = out_dir
        .ancestors()
        .nth(3)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("cannot derive Cargo profile from OUT_DIR {out_dir:?}"))?;
    let profile = if directory == "debug" {
        "dev"
    } else {
        directory
    };
    if profile.is_empty()
        || !profile.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(format!(
            "Cargo selected a non-portable profile directory {directory:?}"
        ));
    }
    Ok(profile)
}

fn main() -> Result<(), String> {
    println!("cargo:rerun-if-changed=build.rs");
    let out_dir = env::var_os("OUT_DIR")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "Cargo did not provide OUT_DIR".to_owned())?;
    let profile = selected_profile(&out_dir)?;
    println!("cargo:rustc-env={PROFILE_ENV}={profile}");
    Ok(())
}
