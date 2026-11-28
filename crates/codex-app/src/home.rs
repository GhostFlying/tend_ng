use anyhow::{Context, Result, ensure};
use std::{
    env, fs, io,
    path::{Component, Path, PathBuf},
};

const SESSION_ROOTS: [&str; 2] = ["sessions", "archived_sessions"];

fn user_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// Resolve existing ancestors even if Codex has not created the directory yet.
fn resolve(path: &Path) -> Result<PathBuf> {
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        match cursor.canonicalize() {
            Ok(mut prefix) => {
                prefix.extend(missing.iter().rev());
                return Ok(prefix);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(cursor.file_name().context("invalid Codex home path")?);
                cursor = cursor.parent().context("invalid Codex home path")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn reject_personal(home: &Path, user_home: Option<&Path>) -> Result<()> {
    if let Some(user_home) = user_home {
        let personal = user_home.join(".codex");
        let resolved = resolve(&personal)?;
        ensure!(
            [&personal, &resolved]
                .into_iter()
                .all(|path| !home.starts_with(path) && !path.starts_with(home)),
            "CODEX_HOME must be separate from the personal Codex home"
        );
    }
    Ok(())
}

fn canonical_service_path(path: &Path, user_home: Option<&Path>) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "CODEX_HOME must be an absolute service path"
    );
    let home = resolve(path)?;
    reject_personal(&home, user_home)?;
    Ok(home)
}

/// Validate an existing home before using it for App Server startup.
pub fn validate_home(path: &Path) -> Result<PathBuf> {
    validate_service_home(path, user_home().as_deref())
}

fn validate_service_home(path: &Path, user_home: Option<&Path>) -> Result<PathBuf> {
    let home = canonical_service_path(path, user_home)?;
    ensure!(home.is_dir(), "CODEX_HOME is not a directory");
    session_roots(&home)?;
    Ok(home)
}

/// Prepare an explicit service home without falling back to the interactive
/// Codex profile. Check existing ancestors before creating any directory.
pub fn prepare_home(path: &Path) -> Result<PathBuf> {
    prepare_service_home(path, user_home().as_deref())
}

fn prepare_service_home(path: &Path, user_home: Option<&Path>) -> Result<PathBuf> {
    let home = canonical_service_path(path, user_home)?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.recursive(true).create(&home)?;
    ensure!(
        validate_service_home(&home, user_home)? == home,
        "CODEX_HOME changed while preparing it"
    );
    Ok(home)
}

/// Use the same layout for new writers and historical readers. Codex creates
/// missing directories itself. A symlink or non-directory is not a session root.
pub(crate) fn session_roots(home: &Path) -> io::Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for name in SESSION_ROOTS {
        let path = home.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => roots.push(path),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("CODEX_HOME/{name} must be a directory, not a symlink or other file"),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(roots)
}

/// Recover the original home from a recorded absolute rollout path. Inspect
/// its components before resolving any symlinks so a moved root cannot change
/// the binding to the home at its target.
pub fn rollout_home(path: &Path) -> Option<&Path> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    path.parent()?
        .ancestors()
        .find(|dir| {
            dir.file_name()
                .is_some_and(|name| SESSION_ROOTS.iter().any(|root| name == *root))
        })?
        .parent()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn fixture(test: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
        let root = env::temp_dir().join(format!("tend-home-{}", protocol::new_id()));
        fs::create_dir(&root)?;
        let result = test(&root);
        fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn service_home_cannot_overlap_personal_alias_or_target() -> Result<()> {
        fixture(|root| {
            let user = root.join("user");
            fs::create_dir(&user)?;
            let personal = root.join("personal-target");
            fs::create_dir(&personal)?;
            let alias = user.join(".codex");
            std::os::unix::fs::symlink(&personal, &alias)?;
            for bad in [
                &user,
                &alias,
                &personal,
                &personal.join("child"),
                &alias.join("child"),
            ] {
                assert!(prepare_service_home(bad, Some(&user)).is_err(), "{bad:?}");
            }
            assert!(!personal.join("child").exists());
            let dedicated = root.join("service");
            assert_eq!(
                prepare_service_home(&dedicated, Some(&user))?,
                dedicated.canonicalize()?
            );
            assert!(prepare_service_home(Path::new("relative"), Some(&user)).is_err());
            Ok(())
        })
    }

    #[test]
    fn session_layout_is_identical_for_launch_and_history() -> Result<()> {
        use protocol::TokenSourceGap;
        fixture(|root| {
            let home = prepare_service_home(&root.join("service"), None)?;
            assert!(crate::usage::rollout_index(&home).is_ok());
            let outside = root.join("elsewhere");
            fs::create_dir(&outside)?;
            for name in SESSION_ROOTS {
                let path = home.join(name);
                std::os::unix::fs::symlink(&outside, &path)?;
                assert!(prepare_service_home(&home, None).is_err());
                assert!(validate_service_home(&home, None).is_err());
                assert_eq!(
                    crate::usage::rollout_index(&home),
                    Err(TokenSourceGap::SourceUnreadable)
                );
                fs::remove_file(&path)?;
                fs::write(&path, "")?;
                assert!(validate_service_home(&home, None).is_err());
                assert_eq!(
                    crate::usage::rollout_index(&home),
                    Err(TokenSourceGap::SourceUnreadable)
                );
                fs::remove_file(&path)?;
                fs::create_dir(&path)?;
                assert!(validate_service_home(&home, None).is_ok());
                assert!(crate::usage::rollout_index(&home).is_ok());
            }
            Ok(())
        })
    }
}
