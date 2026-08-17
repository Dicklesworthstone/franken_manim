use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=FMN_BUILD_ID");
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest dir"));
    let repo = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("fmn-cli remains under crates/");
    let build_id = env::var("FMN_BUILD_ID")
        .ok()
        .filter(|value| valid_identity(value))
        .or_else(|| git_commit(repo))
        .unwrap_or_else(|| {
            format!(
                "release:{}:{}",
                env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "fmn-cli".to_owned()),
                env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_owned())
            )
        });
    println!("cargo:rustc-env=FMN_BUILD_ID={build_id}");
    println!(
        "cargo:rustc-env=FMN_TARGET_TRIPLE={}",
        env::var("TARGET").expect("Cargo target triple")
    );
    let out_dir = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .expect("Cargo OUT_DIR");
    println!(
        "cargo:rustc-env=FMN_CARGO_PROFILE={}",
        selected_profile(&out_dir).expect("portable Cargo profile directory")
    );
}

fn git_commit(repo: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(repo)?;
    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    let head = fs::read_to_string(&head_path).ok()?;
    let head = head.trim();
    let hash = if let Some(reference) = head.strip_prefix("ref: ") {
        let reference_path = git_dir.join(reference);
        println!("cargo:rerun-if-changed={}", reference_path.display());
        fs::read_to_string(&reference_path)
            .ok()
            .map(|value| value.trim().to_owned())
            .or_else(|| packed_ref(&git_dir, reference))?
    } else {
        head.to_owned()
    };
    valid_git_hash(&hash).then(|| format!("git:{hash}"))
}

fn resolve_git_dir(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let marker = fs::read_to_string(dot_git).ok()?;
    let target = marker.trim().strip_prefix("gitdir: ")?;
    let path = PathBuf::from(target);
    Some(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
}

fn packed_ref(git_dir: &Path, reference: &str) -> Option<String> {
    let path = git_dir.join("packed-refs");
    println!("cargo:rerun-if-changed={}", path.display());
    fs::read_to_string(path).ok()?.lines().find_map(|line| {
        let (hash, name) = line.split_once(' ')?;
        (name == reference && valid_git_hash(hash)).then(|| hash.to_owned())
    })
}

fn valid_git_hash(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'\'' | b'"' | b'\\'))
}
