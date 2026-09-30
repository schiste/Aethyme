//! Explicit operator consent for exact-path implicit lease overlaps.
//!
//! A thin client of the broker's public typed API. It never edits the database
//! directly, closes sessions, touches source files, or changes Git refs. Normal
//! claim and guarded-exec conflict checks remain unchanged. Foreign explicit
//! leases must be released through the normal broker before this command runs.
//! Apply requires a matching plan digest, current source fingerprints, verified
//! preservation evidence, and a recorded operator authorization reason.
use aethyme_broker::{BrokerStore, LeaseKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Owner {
    worktree: String,
    head: String,
    branch: String,
    files: BTreeMap<String, Option<String>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Conflict {
    path: String,
    session: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Plan {
    schema: u32,
    repository: String,
    session: i64,
    paths: Vec<String>,
    target: Owner,
    owners: BTreeMap<i64, Owner>,
    conflicts: Vec<Conflict>,
}
#[derive(Deserialize)]
struct PreservedOwner {
    worktree: String,
    head: String,
    files: BTreeMap<String, Option<String>>,
}

fn hash(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
fn digest(plan: &Plan) -> Result<String> {
    Ok(hash(&serde_json::to_vec(plan)?))
}
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("CHAU7_CTO_OPTIM_ACTIVE", "1")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Git identity inspection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
fn common(root: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .canonicalize()?)
}
fn normalize(paths: &[String]) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for name in paths {
        let p = Path::new(name);
        if name.is_empty()
            || name.ends_with('/')
            || name.contains(['*', '?', '[', ']', '\\'])
            || p.is_absolute()
            || p.components().any(|c| !matches!(c, Component::Normal(_)))
            || p.components().any(|c| c.as_os_str() == ".git")
            || name.starts_with(".aethyme/broker.")
        {
            return Err(format!("Only literal repository file paths are permitted: {name}").into());
        }
        let canonical = p
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if canonical != *name {
            return Err("Require canonical file paths".into());
        }
        result.push(name.clone());
    }
    result.sort();
    result.dedup();
    if result.is_empty() || result.len() > 128 {
        return Err("Require 1..128 exact file paths".into());
    }
    Ok(result)
}
fn fingerprint(root: &Path, name: &str) -> Result<Option<String>> {
    let mut current = root.to_path_buf();
    for component in Path::new(name).components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("Symlinks are not permitted in handoff paths".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    if !current.is_file() {
        return Err(format!("Handoff path is not a regular file: {name}").into());
    }
    Ok(Some(hash(&fs::read(current)?)))
}
fn owner(store: &BrokerStore, id: i64, repository: &Path, paths: &[String]) -> Result<Owner> {
    let session = store.session(id)?;
    if session.status.is_closed() {
        return Err(format!("Session {id} is closed").into());
    }
    let root = PathBuf::from(&session.worktree_path).canonicalize()?;
    if common(&root)? != repository {
        return Err("Session belongs to a different Git repository".into());
    }
    let head = git(&root, &["rev-parse", "HEAD"])?;
    let branch = git(&root, &["branch", "--show-current"])?;
    if branch != session.branch {
        return Err(format!("Session {id} branch changed; reconcile its identity first").into());
    }
    let mut files = BTreeMap::new();
    for name in paths {
        files.insert(name.clone(), fingerprint(&root, name)?);
    }
    if git(&root, &["rev-parse", "HEAD"])? != head {
        return Err("HEAD moved during source inspection".into());
    }
    Ok(Owner {
        worktree: root.to_string_lossy().into_owned(),
        head,
        branch,
        files,
    })
}
fn overlaps(lease: &str, file: &str) -> bool {
    lease == file || (lease.ends_with('/') && file.starts_with(lease))
}
fn plan(store: &BrokerStore, cwd: &Path, session: i64, paths: &[String]) -> Result<Plan> {
    let paths = normalize(paths)?;
    let repository = common(cwd)?;
    let target = owner(store, session, &repository, &paths)?;
    if Path::new(&target.worktree) != cwd.canonicalize()? {
        return Err("Run from the target session worktree".into());
    }
    let mut conflicts = Vec::new();
    let mut owner_paths: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for lease in store.active_leases()? {
        if lease.session_id == session {
            continue;
        }
        for name in &paths {
            if !overlaps(&lease.path, name) {
                continue;
            }
            if lease.kind == LeaseKind::Explicit {
                return Err(format!("Foreign explicit lease remains on {name}; release through the normal broker first").into());
            }
            conflicts.push(Conflict {
                path: name.clone(),
                session: lease.session_id,
            });
            owner_paths
                .entry(lease.session_id)
                .or_default()
                .push(name.clone());
        }
    }
    conflicts.sort_by(|a, b| (&a.path, a.session).cmp(&(&b.path, b.session)));
    conflicts.dedup();
    let mut owners = BTreeMap::new();
    for (id, names) in owner_paths {
        owners.insert(id, owner(store, id, &repository, &normalize(&names)?)?);
    }
    Ok(Plan {
        schema: 1,
        repository: repository.to_string_lossy().into_owned(),
        session,
        paths,
        target,
        owners,
        conflicts,
    })
}
fn verify_preservation(plan: &Plan, manifest: &Path) -> Result<()> {
    let preserved: BTreeMap<i64, PreservedOwner> = serde_json::from_slice(&fs::read(manifest)?)?;
    let directory = manifest
        .parent()
        .ok_or("Preservation manifest has no parent directory")?;
    for (id, source) in &plan.owners {
        let saved = preserved
            .get(id)
            .ok_or("Missing owner preservation evidence")?;
        if Path::new(&saved.worktree).canonicalize()? != Path::new(&source.worktree)
            || saved.head != source.head
        {
            return Err("Preserved owner identity does not match current plan".into());
        }
        for (name, value) in &source.files {
            if saved.files.get(name) != Some(value) {
                return Err("Preserved source fingerprint mismatch".into());
            }
            if let Some(expected) = value {
                let file = directory.join(id.to_string()).join("files").join(name);
                if hash(&fs::read(file)?) != *expected {
                    return Err("Preserved file bytes do not match the approved plan".into());
                }
            }
        }
    }
    Ok(())
}
fn apply(
    store: &mut BrokerStore,
    cwd: &Path,
    approved: &Plan,
    confirm: &str,
    reason: &str,
    manifest: &Path,
) -> Result<()> {
    if reason.trim().is_empty() || reason.len() > 1024 {
        return Err("Require a concise operator authorization reason".into());
    }
    if digest(approved)? != confirm {
        return Err("Plan confirmation digest mismatch".into());
    }
    let current = plan(store, cwd, approved.session, &approved.paths)?;
    if current != *approved {
        return Err("Owner identity, content, or leases changed; review a new plan".into());
    }
    verify_preservation(approved, manifest)?;
    let payload = serde_json::json!({"schema": 1, "digest": confirm, "reason": reason, "paths": approved.paths, "owners": approved.owners.keys().collect::<Vec<_>>()});
    store.append_event(
        "broker.lease.operator_handoff.authorized",
        Some(approved.session),
        Some(&payload.to_string()),
    )?;
    // The public typed API records each acquisition in the append-only ledger.
    // Keep foreign implicit telemetry. Never release a foreign explicit lease
    // or mask unconsented changes; guarded exec still rejects explicit races.
    let existing = store
        .active_leases()?
        .into_iter()
        .filter(|lease| lease.session_id == approved.session && lease.kind == LeaseKind::Explicit)
        .map(|lease| lease.path)
        .collect::<std::collections::BTreeSet<_>>();
    let mut acquired: Vec<String> = Vec::new();
    for name in &approved.paths {
        let refreshed = plan(store, cwd, approved.session, &approved.paths)?;
        if refreshed != *approved {
            for old in acquired {
                store.release_lease(approved.session, &old)?;
            }
            return Err(
                "Source changed during ownership acquisition; acquisitions rolled back".into(),
            );
        }
        if existing.contains(name) {
            continue;
        }
        if let Err(error) = store.claim_lease(approved.session, name, None) {
            for old in acquired {
                store.release_lease(approved.session, &old)?;
            }
            return Err(error.into());
        }
        acquired.push(name.clone());
    }
    if plan(store, cwd, approved.session, &approved.paths)? != *approved {
        for old in acquired {
            store.release_lease(approved.session, &old)?;
        }
        return Err("Concurrent ownership or source change; acquisitions rolled back".into());
    }
    store.append_event(
        "broker.lease.operator_handoff.completed",
        Some(approved.session),
        Some(&payload.to_string()),
    )?;
    Ok(())
}
fn value(args: &[String], name: &str) -> Result<String> {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].clone())
        .ok_or_else(|| format!("Missing {name}").into())
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        println!(
            "Reviewed operator lease handoff\n\nplan --repo <worktree> --session <id> --paths-file <file> --output <file>\napply --repo <worktree> --plan <file> --confirm <sha256> --reason <operator-authorization> --preservation-manifest <file>\n\nForeign explicit ownership must be released through the normal broker. Source/HEAD changes and invalid preservation evidence refuse the handoff."
        );
        return Ok(());
    }
    if args.len() == 1 && args[0] == "--version" {
        println!("broker-lease-handoff {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let cwd = PathBuf::from(value(&args, "--repo")?).canonicalize()?;
    let repository = common(&cwd)?;
    if repository.file_name().is_none_or(|name| name != ".git") {
        return Err("Require a standard non-bare Git repository".into());
    }
    let root = repository.parent().ok_or("Missing repository root")?;
    let mut store = BrokerStore::open_in_repo(root)?;
    match args.first().map(String::as_str) {
        Some("plan") => {
            let session = value(&args,"--session")?.parse()?;
            let paths = fs::read_to_string(value(&args,"--paths-file")?)?.lines().map(str::to_string).collect::<Vec<_>>();
            let result = plan(&store,&cwd,session,&paths)?;
            let output = PathBuf::from(value(&args,"--output")?);
            fs::write(&output,serde_json::to_vec_pretty(&result)?)?;
            println!("Reviewed operator handoff: {} exact paths, {} implicit overlaps. Plan: {}. Confirmation: {}", result.paths.len(),result.conflicts.len(),output.display(),digest(&result)?);
        },
        Some("apply") => {
            let approved: Plan = serde_json::from_slice(&fs::read(value(&args,"--plan")?)?)?;
            apply(&mut store,&cwd,&approved,&value(&args,"--confirm")?,&value(&args,"--reason")?,Path::new(&value(&args,"--preservation-manifest")?))?;
            println!("Operator handoff completed for session {}: {} exact paths. Other source files, refs and sessions preserved.",approved.session,approved.paths.len());
        },
        _ => return Err("Use plan --repo <worktree> --session <id> --paths-file <file> --output <file>, then apply --repo <worktree> --plan <file> --confirm <sha256> --reason <authorization> --preservation-manifest <file>".into()),
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Operator handoff refused: {error}");
        std::process::exit(3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aethyme_broker::{NewSession, SessionOrigin, SessionStatus};
    use tempfile::TempDir;
    fn command(root: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    struct Fixture {
        _temp: TempDir,
        root: PathBuf,
        owner: PathBuf,
        store: BrokerStore,
        actor: i64,
        holder: i64,
        manifest: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let root = temp.path().join("repo");
            fs::create_dir(&root).unwrap();
            command(&root, &["init", "-q", "-b", "main"]);
            command(&root, &["config", "user.email", "test@example.invalid"]);
            command(&root, &["config", "user.name", "Operator tests"]);
            fs::write(root.join("policy.yml"), "policy\n").unwrap();
            command(&root, &["add", "policy.yml"]);
            command(&root, &["commit", "-qm", "baseline"]);
            let holder_root = temp.path().join("holder");
            command(
                &root,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "holder",
                    holder_root.to_str().unwrap(),
                ],
            );
            let mut store = BrokerStore::open_in_repo(&root).unwrap();
            fn session(store: &mut BrokerStore, wd: &Path, branch: &str) -> i64 {
                store
                    .register_session(&NewSession {
                        worktree_path: wd.canonicalize().unwrap().to_string_lossy().into_owned(),
                        branch: branch.into(),
                        origin: SessionOrigin::Adopted,
                        task: Some("operator test".into()),
                        diff_base: None,
                        adoption_base: None,
                        adopted_head: None,
                        repository_contract: None,
                        pid: None,
                        command: None,
                        log_path: None,
                        agent_identity: None,
                    })
                    .unwrap()
                    .id
            }
            let actor = session(&mut store, &root, "main");
            let holder = session(&mut store, &holder_root, "holder");
            store
                .set_implicit_leases(holder, &["policy.yml".into()])
                .unwrap();
            let manifest = temp.path().join("manifest.json");
            let result = plan(&store, &root, actor, &["policy.yml".into()]).unwrap();
            let source = result.owners.get(&holder).unwrap();
            let saved = serde_json::json!({holder.to_string():{"worktree":source.worktree,"head":source.head,"files":source.files}});
            fs::write(&manifest, serde_json::to_vec(&saved).unwrap()).unwrap();
            let backup = temp.path().join(holder.to_string()).join("files");
            fs::create_dir_all(&backup).unwrap();
            fs::copy(holder_root.join("policy.yml"), backup.join("policy.yml")).unwrap();
            Self {
                _temp: temp,
                root,
                owner: holder_root,
                store,
                actor,
                holder,
                manifest,
            }
        }
        fn plan(&self) -> Plan {
            plan(&self.store, &self.root, self.actor, &["policy.yml".into()]).unwrap()
        }
        fn explicit(&self) -> usize {
            self.store
                .active_leases()
                .unwrap()
                .iter()
                .filter(|l| l.session_id == self.actor && l.kind == LeaseKind::Explicit)
                .count()
        }
    }
    #[test]
    fn acquires_reviewed_implicit_overlap_without_changing_owner_work_or_session() {
        let mut f = Fixture::new();
        let p = f.plan();
        let bytes = fs::read(f.owner.join("policy.yml")).unwrap();
        apply(
            &mut f.store,
            &f.root,
            &p,
            &digest(&p).unwrap(),
            "User authorized scoped handoff",
            &f.manifest,
        )
        .unwrap();
        assert_eq!(f.explicit(), 1);
        assert_eq!(fs::read(f.owner.join("policy.yml")).unwrap(), bytes);
        assert!(!f.store.session(f.holder).unwrap().status.is_closed());
        assert!(
            f.store
                .active_leases()
                .unwrap()
                .iter()
                .any(|l| l.session_id == f.holder && l.kind == LeaseKind::Implicit)
        );
    }
    #[test]
    fn rejects_unreleased_foreign_explicit_ownership() {
        let mut f = Fixture::new();
        f.store.claim_lease(f.holder, "policy.yml", None).unwrap();
        assert!(
            plan(&f.store, &f.root, f.actor, &["policy.yml".into()])
                .unwrap_err()
                .to_string()
                .contains("Foreign explicit")
        );
        assert_eq!(f.explicit(), 0);
    }
    #[test]
    fn rejects_stale_owner_content_and_keeps_it_intact() {
        let mut f = Fixture::new();
        let p = f.plan();
        fs::write(f.owner.join("policy.yml"), "new WIP\n").unwrap();
        assert!(
            apply(
                &mut f.store,
                &f.root,
                &p,
                &digest(&p).unwrap(),
                "approved",
                &f.manifest
            )
            .is_err()
        );
        assert_eq!(f.explicit(), 0);
        assert_eq!(
            fs::read_to_string(f.owner.join("policy.yml")).unwrap(),
            "new WIP\n"
        );
    }
    #[test]
    fn rejects_bad_confirmation_and_missing_authorization() {
        let mut f = Fixture::new();
        let p = f.plan();
        assert!(apply(&mut f.store, &f.root, &p, "bad", "approved", &f.manifest).is_err());
        assert!(
            apply(
                &mut f.store,
                &f.root,
                &p,
                &digest(&p).unwrap(),
                " ",
                &f.manifest
            )
            .is_err()
        );
        assert_eq!(f.explicit(), 0);
    }
    #[test]
    fn rejects_corrupt_preservation_before_claiming() {
        let mut f = Fixture::new();
        let p = f.plan();
        let path = f
            .manifest
            .parent()
            .unwrap()
            .join(f.holder.to_string())
            .join("files/policy.yml");
        fs::write(path, "corrupt backup").unwrap();
        assert!(
            apply(
                &mut f.store,
                &f.root,
                &p,
                &digest(&p).unwrap(),
                "approved",
                &f.manifest
            )
            .is_err()
        );
        assert_eq!(f.explicit(), 0);
    }
    #[test]
    fn preserves_existing_target_lease_on_success() {
        let mut f = Fixture::new();
        let previous = f
            .store
            .claim_lease(f.actor, "policy.yml", Some(1_000_000))
            .unwrap();
        let p = f.plan();
        apply(
            &mut f.store,
            &f.root,
            &p,
            &digest(&p).unwrap(),
            "approved",
            &f.manifest,
        )
        .unwrap();
        let current = f
            .store
            .active_leases()
            .unwrap()
            .into_iter()
            .find(|l| l.id == previous.id)
            .unwrap();
        assert_eq!(current.expires_at, previous.expires_at);
        assert_eq!(current.created_at, previous.created_at);
    }
    #[test]
    fn rejects_other_worktree_invocation() {
        let f = Fixture::new();
        assert!(plan(&f.store, &f.owner, f.actor, &["policy.yml".into()]).is_err());
    }
    #[test]
    fn rejects_directory_explicit_ownership() {
        let mut f = Fixture::new();
        f.store.claim_lease(f.holder, "dir/", None).unwrap();
        assert!(plan(&f.store, &f.root, f.actor, &["dir/policy.yml".into()]).is_err());
        assert!(!overlaps("scripts/", "scripts-other/policy.yml"));
    }
    #[test]
    fn rejects_closed_target_session() {
        let mut f = Fixture::new();
        f.store
            .set_session_status(f.actor, SessionStatus::Closed, None)
            .unwrap();
        assert!(plan(&f.store, &f.root, f.actor, &["policy.yml".into()]).is_err());
    }
    #[test]
    fn rejects_directory_glob_parent_and_git_metadata_paths() {
        for path in [
            "scripts/",
            "scripts/*",
            "scripts//a",
            "scripts/./a",
            "../policy.yml",
            "/policy.yml",
            ".git/config",
            ".aethyme/broker.db",
            "",
        ] {
            assert!(normalize(&[path.into()]).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_paths() {
        let f = Fixture::new();
        std::os::unix::fs::symlink(f.root.join("policy.yml"), f.root.join("linked")).unwrap();
        assert!(fingerprint(&f.root, "linked").is_err());
    }
}
