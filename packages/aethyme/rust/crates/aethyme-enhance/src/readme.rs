//! Preserve repository prose while refreshing one managed learning section.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::deploy::{ensure_no_symlink_in_path, DeployAction};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
const MAX_README_BYTES: u64 = 4 * 1024 * 1024;

fn read_bounded(path: &Path) -> Result<String, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_README_BYTES {
        return Err(format!(
            "{}: README must be a regular file no larger than 4 MiB",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_README_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_README_BYTES {
        return Err("README grew beyond the 4 MiB limit while reading".into());
    }
    String::from_utf8(bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn readme_path(repo: &Path) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(repo).map_err(|e| format!("{}: {e}", repo.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "readme" | "readme.md" | "readme.markdown" | "readme.rst" | "readme.txt"
        ) {
            candidates.push(entry.path());
        }
    }
    match candidates.len() {
        0 => Ok(repo.join("README.md")),
        1 => Ok(candidates.remove(0)),
        _ => Err("multiple README files found; select one primary README before refreshing skill guidance".into()),
    }
}

fn managed_block(rst: bool, newline: &str) -> (String, String, String) {
    let (begin, end, heading) = if rst {
        (
            ".. AETHYME:BEGIN skill-learning",
            ".. AETHYME:END skill-learning",
            "Skill learning and maintenance\n==============================",
        )
    } else {
        (
            "<!-- AETHYME:BEGIN skill-learning -->",
            "<!-- AETHYME:END skill-learning -->",
            "## Skill learning and maintenance",
        )
    };
    let guidance = "Skills are editable procedures. After meaningful work, review confirmed fixes,\nuser corrections, pitfalls, and repeatable techniques for a reusable lesson.\n\n- Read the relevant existing skill before changing it; prefer the skill used\n  for the work. Correct inaccurate guidance and add missing durable steps.\n- Keep skills focused on reusable procedures. Exclude session transcripts,\n  secrets, one-off details, and unconfirmed guesses.\n- Create a new skill only for a reusable class of work with no suitable home.\n- Follow this repository's skill conventions in `.codex/skills/` and\n  `.claude/skills/`; keep matching mirrors aligned where the repository\n  requires it. Respect ownership and do not edit system or plugin caches.";
    let guidance = if rst {
        guidance.replace('`', "``")
    } else {
        guidance.to_string()
    };
    let block = format!("{begin}\n\n{heading}\n\n{guidance}\n\n{end}").replace('\n', newline);
    (begin.into(), end.into(), block)
}

fn render(existing: &str, rst: bool) -> Result<String, String> {
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let (begin, end, block) = managed_block(rst, newline);
    match (existing.matches(&begin).count(), existing.matches(&end).count()) {
        (0, 0) => {
            let separator = if existing.is_empty() { "".to_string() } else if existing.ends_with(newline) { newline.into() } else { format!("{newline}{newline}") };
            Ok(format!("{existing}{separator}{block}{newline}"))
        }
        (1, 1) => {
            let start = existing.find(&begin).expect("one begin marker");
            let finish = existing.find(&end).expect("one end marker");
            if finish < start { return Err("README skill-learning markers are out of order; existing content was preserved".into()); }
            Ok(format!("{}{}{}", &existing[..start], block, &existing[finish + end.len()..]))
        }
        _ => Err("README skill-learning markers are incomplete or duplicated; existing content was preserved".into()),
    }
}

/// Add or refresh the owned section, leaving every byte outside it intact.
/// Canonical deployment and initialization share this update path; local-only
/// deployment intentionally does not modify a repository README.
pub fn ensure_guidance(repo: &Path) -> Result<DeployAction, String> {
    let path = readme_path(repo)?;
    ensure_no_symlink_in_path(repo, &path)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() && m.len() <= MAX_README_BYTES => Some(m),
        Ok(_) => {
            return Err(format!(
                "{}: README must be a regular file no larger than 4 MiB",
                path.display()
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let existing = if metadata.is_some() {
        read_bounded(&path)?
    } else {
        String::new()
    };
    let rst = path
        .extension()
        .is_some_and(|e| e.to_str().is_some_and(|e| e.eq_ignore_ascii_case("rst")));
    let content = render(&existing, rst)?;
    let relative_path = path
        .file_name()
        .expect("README filename")
        .to_string_lossy()
        .into_owned();
    if metadata.is_some() && content == existing {
        return Ok(DeployAction {
            relative_path,
            action: "unchanged",
        });
    }
    let temporary = repo.join(format!(
        ".aethyme-readme-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| format!("{}: {e}", temporary.display()))?;
    let result = (|| -> Result<(), String> {
        if let Some(m) = &metadata {
            output
                .set_permissions(m.permissions())
                .map_err(|e| e.to_string())?;
        }
        output
            .write_all(content.as_bytes())
            .map_err(|e| e.to_string())?;
        output.sync_all().map_err(|e| e.to_string())?;
        drop(output);
        ensure_no_symlink_in_path(repo, &path)?;
        match (&metadata, read_bounded(&path)) {
            (Some(_), Ok(current)) if current == existing => {}
            (None, Err(_)) if matches!(fs::symlink_metadata(&path), Err(e) if e.kind() == std::io::ErrorKind::NotFound) =>
                {}
            _ => return Err(
                "README changed while preparing guidance; retry after reviewing the current file"
                    .into(),
            ),
        }
        fs::rename(&temporary, &path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(())
    })();
    if let Err(error) = result {
        if let Err(cleanup) = fs::remove_file(&temporary) {
            return Err(format!(
                "{error}; temporary file {} could not be removed: {cleanup}",
                temporary.display()
            ));
        }
        return Err(error);
    }
    Ok(DeployAction {
        relative_path,
        action: if metadata.is_some() {
            "updated"
        } else {
            "created"
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo(PathBuf);
    impl Repo {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "aethyme-readme-test-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Repo {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn preserves_prose_and_repeated_updates_are_identical() {
        let original = "# Product\n\nUnrelated prose, Unicode: é. No final newline";
        let first = render(original, false).unwrap();
        assert!(first.starts_with(original));
        assert_eq!(first, render(&first, false).unwrap());
        assert_eq!(
            first
                .matches("<!-- AETHYME:BEGIN skill-learning -->")
                .count(),
            1
        );
        for required in [
            "confirmed fixes",
            "user corrections",
            "Read the relevant existing skill",
            "session transcripts",
            ".codex/skills/",
            ".claude/skills/",
            "plugin caches",
        ] {
            assert!(first.contains(required), "{required}");
        }
    }

    #[test]
    fn refresh_replaces_only_owned_bytes_and_preserves_crlf() {
        let existing = "intro\r\n<!-- AETHYME:BEGIN skill-learning -->\r\nold guidance\r\n<!-- AETHYME:END skill-learning -->\r\nsuffix";
        let updated = render(existing, false).unwrap();
        assert!(updated.starts_with("intro\r\n"));
        assert!(updated.ends_with("\r\nsuffix"));
        assert!(!updated.contains("old guidance"));
        assert!(!updated.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn ambiguous_markers_are_refused() {
        for content in ["<!-- AETHYME:BEGIN skill-learning -->", "<!-- AETHYME:END skill-learning -->", "<!-- AETHYME:END skill-learning --><!-- AETHYME:BEGIN skill-learning -->", "<!-- AETHYME:BEGIN skill-learning --><!-- AETHYME:BEGIN skill-learning --><!-- AETHYME:END skill-learning -->"] { assert!(render(content, false).is_err()); }
    }

    #[test]
    fn creates_updates_and_reports_unchanged() {
        let repo = Repo::new();
        assert_eq!(ensure_guidance(&repo.0).unwrap().action, "created");
        let p = repo.0.join("README.md");
        let first = fs::read(&p).unwrap();
        assert_eq!(ensure_guidance(&repo.0).unwrap().action, "unchanged");
        assert_eq!(fs::read(&p).unwrap(), first);
        fs::write(&p, "User prose\n").unwrap();
        assert_eq!(ensure_guidance(&repo.0).unwrap().action, "updated");
        assert!(fs::read_to_string(p).unwrap().starts_with("User prose\n"));
    }

    #[test]
    fn existing_rst_is_used_without_creating_a_shadow_readme() {
        let repo = Repo::new();
        let p = repo.0.join("readme.RST");
        fs::write(&p, "Product\n=======\n").unwrap();
        assert_eq!(
            ensure_guidance(&repo.0).unwrap().relative_path,
            "readme.RST"
        );
        let first = fs::read_to_string(&p).unwrap();
        assert!(first.starts_with("Product\n=======\n"));
        assert!(first.contains(".. AETHYME:BEGIN skill-learning"));
        assert!(first.contains("``.codex/skills/``"));
        assert!(!repo.0.join("README.md").exists());
        assert_eq!(ensure_guidance(&repo.0).unwrap().action, "unchanged");
    }

    #[test]
    fn multiple_readmes_and_nonregular_targets_are_preserved() {
        let repo = Repo::new();
        fs::write(repo.0.join("README.md"), "A").unwrap();
        fs::write(repo.0.join("README.rst"), "B").unwrap();
        assert!(ensure_guidance(&repo.0).is_err());
        assert_eq!(fs::read_to_string(repo.0.join("README.md")).unwrap(), "A");
        fs::remove_file(repo.0.join("README.rst")).unwrap();
        fs::remove_file(repo.0.join("README.md")).unwrap();
        fs::create_dir(repo.0.join("README.md")).unwrap();
        assert!(ensure_guidance(&repo.0).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_read_refuses_fifo_without_waiting_for_a_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let repo = Repo::new();
        let path = repo.0.join("README.md");
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live NUL-terminated path; mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_bounded(&path).unwrap_err().contains("regular file"));
        assert!(ensure_guidance(&repo.0).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_without_touching_the_destination() {
        let repo = Repo::new();
        let target = repo.0.join("outside.txt");
        fs::write(&target, "Keep").unwrap();
        std::os::unix::fs::symlink(&target, repo.0.join("README.md")).unwrap();
        assert!(ensure_guidance(&repo.0).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "Keep");
    }
}
