//! Explicit native-project input roots. No Cargo dependency guessing or symlink
//! following: SourceWatch remains the authority for bounded filesystem reads.

use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};

pub(super) const MAX_EXTRA_ROOTS: usize = 128;
const DEFAULT_ROOTS: [&str; 8] = [
    "Cargo.toml",
    "Cargo.lock",
    "src",
    "examples",
    "build.rs",
    ".cargo",
    "rust-toolchain",
    "rust-toolchain.toml",
];

pub(super) fn roots(project: &Path, extra: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    if !project.is_absolute() || extra.len() > MAX_EXTRA_ROOTS {
        return Err(invalid(
            "source watch needs an absolute project and at most 128 extra roots",
        ));
    }
    let mut roots = BTreeSet::new();
    for path in DEFAULT_ROOTS
        .iter()
        .map(PathBuf::from)
        .chain(extra.iter().cloned())
    {
        if path.as_os_str().is_empty() || path.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(invalid(
                "source-watch paths must be nonempty and contain no NUL",
            ));
        }
        let joined = project.join(path);
        if !joined.is_absolute() {
            return Err(invalid(
                "source-watch path did not resolve to an absolute path",
            ));
        }
        let mut normalized = PathBuf::new();
        for component in joined.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(invalid(
                            "source-watch path traverses above the filesystem root",
                        ));
                    }
                }
                _ => normalized.push(component.as_os_str()),
            }
        }
        roots.insert(normalized);
    }
    Ok(roots.into_iter().collect())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> PathBuf {
        std::env::current_dir()
            .unwrap()
            .join("native-project-input-test")
    }

    #[test]
    fn defaults_cover_examples_build_scripts_and_toolchain_configuration() {
        let project = project();
        let roots = roots(&project, &[]).unwrap();
        assert_eq!(roots.len(), DEFAULT_ROOTS.len());
        for name in DEFAULT_ROOTS {
            assert!(roots.contains(&project.join(name)), "{name}");
        }
        assert!(!roots.contains(&project.join("target")));
        assert!(!roots.contains(&project.join(".git")));
    }

    #[test]
    fn dependency_and_asset_paths_are_relative_to_the_manifest_not_cwd() {
        let project = project();
        let roots = roots(
            &project,
            &["../shared/src".into(), "assets/voice.wav".into()],
        )
        .unwrap();
        assert!(roots.contains(&project.parent().unwrap().join("shared/src")));
        assert!(roots.contains(&project.join("assets/voice.wav")));
    }

    #[test]
    fn explicitly_selected_directories_and_missing_paths_are_retained() {
        let project = project();
        let roots = roots(
            &project,
            &["assets".into(), "not-created-yet/model.json".into()],
        )
        .unwrap();
        assert!(roots.contains(&project.join("assets")));
        assert!(roots.contains(&project.join("not-created-yet/model.json")));
    }

    #[test]
    fn absolute_roots_keep_their_authority() {
        let project = project();
        let external = project.parent().unwrap().join("another-project/Cargo.toml");
        let roots = roots(&project, std::slice::from_ref(&external)).unwrap();
        assert!(roots.contains(&external));
    }

    #[test]
    fn lexical_aliases_are_deduplicated_before_scanning() {
        let project = project();
        let roots = roots(
            &project,
            &["./src".into(), "target/../src".into(), project.join("src")],
        )
        .unwrap();
        assert_eq!(roots.len(), DEFAULT_ROOTS.len());
        assert!(roots.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn invalid_or_unbounded_declarations_refuse_without_filesystem_access() {
        assert!(roots(Path::new("relative"), &[]).is_err());
        assert!(roots(&project(), &[PathBuf::new()]).is_err());
        assert!(roots(&project(), &["bad\0path".into()]).is_err());
        assert!(roots(&project(), &vec!["src".into(); MAX_EXTRA_ROOTS + 1]).is_err());
    }

    #[test]
    fn lexical_parent_traversal_cannot_erase_the_filesystem_root() {
        let project = project();
        let root = project.ancestors().last().unwrap();
        assert!(roots(root, &["../outside".into()]).is_err());
    }
}
