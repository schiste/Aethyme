//! Explicit macOS source-storage pressure, isolated from host and daemon stores.
use super::{NativeClient, age_objects, observe_control};
use crate::collaboration_archive::{self as archive, ObjectDigest};
use crate::collaboration_capture as capture;
use crate::collaboration_state::{CollaborationRoot, CollaborationStore};
use crate::execution_snapshot as snapshot;
use crate::local_target::{
    self as target,
    tests::{Fixture, git},
};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const IMAGE_BYTES: u64 = 512 * 1024 * 1024;
const PROFILE: &str = "owned-apfs-source-pressure/provisional-v0";

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnedMount {
    device: String,
    volume: String,
}

fn device(value: &str, whole: bool) -> bool {
    let Some(rest) = value.strip_prefix("/dev/disk") else {
        return false;
    };
    let parts: Vec<_> = rest.split('s').collect();
    !parts.is_empty()
        && (!whole || parts.len() == 1)
        && parts.len() <= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
}

fn inventory_owner(
    value: &Value,
    image: &Path,
    mount: &Path,
) -> Result<Option<OwnedMount>, String> {
    let images = value
        .get("images")
        .and_then(Value::as_array)
        .ok_or("unknown inventory shape")?;
    if images.iter().any(|i| {
        i.get("image-path")
            .and_then(Value::as_str)
            .is_none_or(|s| s.is_empty())
    }) {
        return Err("unknown image identity cannot prove absence".into());
    }
    let exact: Vec<_> = images
        .iter()
        .filter(|i| {
            i.get("image-path")
                .and_then(Value::as_str)
                .is_some_and(|p| Path::new(p) == image)
        })
        .collect();
    if exact.is_empty() {
        return Ok(None);
    }
    if exact.len() != 1 {
        return Err("ambiguous image identity".into());
    }
    let entries = exact[0]
        .get("system-entities")
        .and_then(Value::as_array)
        .ok_or("unknown entities")?;
    let roots: Vec<_> = entries
        .iter()
        .filter(|e| e["content-hint"] == "GUID_partition_scheme")
        .collect();
    let mounted: Vec<_> = entries
        .iter()
        .filter(|e| {
            e.get("mount-point")
                .and_then(Value::as_str)
                .is_some_and(|p| Path::new(p) == mount)
        })
        .collect();
    if roots.len() != 1 || mounted.len() != 1 {
        return Err("image mount association differs".into());
    }
    let root = roots[0]["dev-entry"]
        .as_str()
        .ok_or("missing image device")?;
    let volume = mounted[0]["dev-entry"]
        .as_str()
        .ok_or("missing volume device")?;
    if !device(root, true) || !device(volume, false) {
        return Err("invalid native device identity".into());
    }
    Ok(Some(OwnedMount {
        device: root.into(),
        volume: volume.into(),
    }))
}

fn check_volume(
    value: &Value,
    binding: &OwnedMount,
    mount: &Path,
    uuid: Option<&str>,
) -> Result<String, String> {
    if value["DeviceNode"] != binding.volume
        || value["MountPoint"]
            .as_str()
            .is_none_or(|p| Path::new(p) != mount)
        || value["FilesystemType"] != "apfs"
    {
        return Err("mounted filesystem identity differs".into());
    }
    let current = value["VolumeUUID"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("missing volume UUID")?;
    if uuid.is_some_and(|u| u != current) {
        return Err("volume UUID differs".into());
    }
    Ok(current.into())
}

struct OwnedImage {
    root: PathBuf,
    image: PathBuf,
    mount: PathBuf,
    commands: usize,
    binding: Option<OwnedMount>,
    uuid: Option<String>,
    image_inode: Option<(u64, u64)>,
    volume_guard: Option<File>,
    volume_device: Option<u64>,
    cleanup_confirmed: bool,
    mutation_unknown: bool,
}
impl OwnedImage {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap().keep().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let image = root.join("owned-source.asif");
        let mount = root.join("owned-volume");
        eprintln!("Native source filesystem recovery root {}", root.display());
        let this = Self {
            root,
            image,
            mount,
            commands: 0,
            binding: None,
            uuid: None,
            image_inode: None,
            volume_guard: None,
            volume_device: None,
            cleanup_confirmed: false,
            mutation_unknown: false,
        };
        this.note("contract", json!({"profile":PROFILE,"capacity_bytes":IMAGE_BYTES,"image":this.image,"mount":this.mount,"cleanup_confirmed":false}));
        this
    }
    fn note(&self, name: &str, value: Value) {
        let path = self.root.join(format!(
            "{}-{name}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        f.write_all(&serde_json::to_vec_pretty(&value).unwrap())
            .unwrap();
        f.sync_all().unwrap();
        File::open(&self.root).unwrap().sync_all().unwrap();
    }
    fn native(&mut self, program: &str, args: &[&str], seconds: u64) -> Result<Output, String> {
        self.commands += 1;
        if self.commands > 128 {
            return Err("native command budget exhausted".into());
        }
        self.note("native-start", json!({"command":self.commands,"program":program,"args":args,"deadline_seconds":seconds,"output_limit_each":1024*1024}));
        let stdout = tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        let stderr = tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout.reopen().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(stderr.reopen().map_err(|e| e.to_string())?))
            .spawn()
            .map_err(|e| e.to_string())?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            observe_control(
                NativeClient {
                    child,
                    reaped: false,
                },
                stdout,
                stderr,
                Instant::now() + Duration::from_secs(seconds),
            )
        }));
        match result {
            Ok(out) => {
                self.note("native-complete",json!({"command":self.commands,"exit":out.status.code(),"stdout_sha256":ObjectDigest::of(&out.stdout).hex(),"stderr_sha256":ObjectDigest::of(&out.stderr).hex()}));
                Ok(out)
            }
            Err(_) => {
                if program == "/usr/sbin/diskutil"
                    && (args.first() == Some(&"eject")
                        || args.starts_with(&["image", "create"])
                        || args.starts_with(&["image", "attach"]))
                {
                    self.mutation_unknown = true;
                }
                self.note("native-ambiguous",json!({"command":self.commands,"cleanup_confirmed":false,"mutation_unknown":self.mutation_unknown}));
                Err("native client completion unconfirmed; reconcile exact image".into())
            }
        }
    }
    fn plist(&mut self, program: &str, args: &[&str]) -> Result<Value, String> {
        let out = self.native(program, args, 10)?;
        if !out.status.success() {
            return Err(format!(
                "native observation failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let file = tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        std::fs::write(file.path(), out.stdout).map_err(|e| e.to_string())?;
        let converted = self.native(
            "/usr/bin/plutil",
            &[
                "-convert",
                "json",
                "-o",
                "-",
                file.path().to_str().ok_or("non-Unicode private path")?,
            ],
            10,
        )?;
        if !converted.status.success() {
            return Err("native plist conversion failed".into());
        }
        serde_json::from_slice(&converted.stdout).map_err(|e| e.to_string())
    }
    fn observe(&mut self) -> Result<Option<OwnedMount>, String> {
        let inventory = self.plist("/usr/bin/hdiutil", &["info", "-plist"])?;
        inventory_owner(&inventory, &self.image, &self.mount)
    }
    fn attach(&mut self) {
        let image = self.image.to_str().unwrap().to_owned();
        let mount = self.mount.to_str().unwrap().to_owned();
        let out = self
            .native(
                "/usr/sbin/diskutil",
                &[
                    "image",
                    "create",
                    "blank",
                    "--format",
                    "ASIF",
                    "--size",
                    "536870912",
                    "--volumeName",
                    "AethymeSourcePressure",
                    "--fs",
                    "APFS",
                    &image,
                ],
                60,
            )
            .unwrap();
        assert!(
            out.status.success(),
            "native image creation failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::set_permissions(&self.image, std::fs::Permissions::from_mode(0o600)).unwrap();
        let meta = std::fs::symlink_metadata(&self.image).unwrap();
        assert!(meta.is_file() && !meta.file_type().is_symlink());
        self.image_inode = Some((meta.dev(), meta.ino()));
        let out = self
            .native(
                "/usr/sbin/diskutil",
                &[
                    "image",
                    "attach",
                    "--nobrowse",
                    "--mountPoint",
                    &mount,
                    &image,
                ],
                60,
            )
            .unwrap();
        assert!(
            out.status.success(),
            "native image attachment failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let binding = self
            .observe()
            .unwrap()
            .expect("native exact image is attached");
        let info = self
            .plist("/usr/sbin/diskutil", &["info", "-plist", &mount])
            .unwrap();
        let uuid = check_volume(&info, &binding, &self.mount, None).unwrap();
        let guard = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&self.mount)
            .unwrap();
        let volume_device = guard.metadata().unwrap().dev();
        assert_ne!(
            volume_device,
            std::fs::metadata(&self.root).unwrap().dev(),
            "pressure mount must differ from host filesystem"
        );
        self.note("mounted",json!({"device":binding.device,"volume":binding.volume,"uuid":uuid,"filesystem_device":volume_device,"image_inode":self.image_inode}));
        self.binding = Some(binding);
        self.uuid = Some(uuid);
        self.volume_guard = Some(guard);
        self.volume_device = Some(volume_device);
        self.assert_mounted();
    }
    fn assert_mounted(&mut self) {
        let current = self
            .observe()
            .unwrap()
            .expect("pressure mount cannot fall back to host");
        assert_eq!(
            Some(&current),
            self.binding.as_ref(),
            "current image association differs"
        );
        let mount = self.mount.to_str().unwrap().to_owned();
        let info = self
            .plist("/usr/sbin/diskutil", &["info", "-plist", &mount])
            .unwrap();
        check_volume(&info, &current, &self.mount, self.uuid.as_deref()).unwrap();
        let image = std::fs::symlink_metadata(&self.image).unwrap();
        assert!(image.is_file() && !image.file_type().is_symlink());
        assert_eq!(Some((image.dev(), image.ino())), self.image_inode);
        assert_eq!(
            Some(std::fs::metadata(&self.mount).unwrap().dev()),
            self.volume_device,
            "pressure destination must remain on owned volume"
        );
        assert_eq!(
            Some(
                self.volume_guard
                    .as_ref()
                    .unwrap()
                    .metadata()
                    .unwrap()
                    .dev()
            ),
            self.volume_device
        );
    }
    fn cleanup(&mut self) -> Result<(), String> {
        if self.mutation_unknown {
            return Err("interrupted image mutation requires operator reconciliation; absence alone cannot settle it".into());
        }
        let current = self.observe()?;
        if let Some(current) = current {
            if self.binding.as_ref() != Some(&current) {
                return Err("exact current image ownership unconfirmed".into());
            }
            self.assert_mounted();
            drop(self.volume_guard.take());
            let out = self.native("/usr/sbin/diskutil", &["eject", &current.device], 60)?;
            if !out.status.success() {
                return Err(format!(
                    "exact owned eject failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }
        if self.observe()?.is_some() {
            return Err("owned image remains attached".into());
        }
        self.cleanup_confirmed = true;
        self.note(
            "cleanup",
            json!({"cleanup_confirmed":true,"image_attached":false,"image_preserved":true}),
        );
        Ok(())
    }
}
impl Drop for OwnedImage {
    fn drop(&mut self) {
        if !self.cleanup_confirmed {
            eprintln!(
                "Unconfirmed native source image cleanup; preserved {}",
                self.root.display()
            );
        }
    }
}

fn enospc(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOSPC)
}

fn inventory(image: &str, mount: &str) -> Value {
    json!({"images":[{"image-path":image,"system-entities":[{"content-hint":"GUID_partition_scheme","dev-entry":"/dev/disk17"},{"dev-entry":"/dev/disk18s1","mount-point":mount}]}]})
}
#[test]
fn owned_image_inventory_requires_unique_path_mount_and_native_device() {
    let image = Path::new("/private/owned.asif");
    let mount = Path::new("/private/owned-volume");
    assert_eq!(
        inventory_owner(
            &inventory(image.to_str().unwrap(), mount.to_str().unwrap()),
            image,
            mount
        )
        .unwrap(),
        Some(OwnedMount {
            device: "/dev/disk17".into(),
            volume: "/dev/disk18s1".into()
        })
    );
    let mut duplicate = inventory(image.to_str().unwrap(), mount.to_str().unwrap());
    let entry = duplicate["images"][0].clone();
    duplicate["images"].as_array_mut().unwrap().push(entry);
    assert!(
        inventory_owner(&duplicate, image, mount).is_err(),
        "duplicate native image association must refuse"
    );
    assert!(
        inventory_owner(
            &inventory(image.to_str().unwrap(), "/private/other-volume"),
            image,
            mount
        )
        .is_err(),
        "wrong mount cannot authorize pressure or cleanup"
    );
    assert!(
        inventory_owner(&json!({}), image, mount).is_err(),
        "unknown inventory cannot prove absence"
    );
    assert!(
        inventory_owner(&json!({"images":[{}]}), image, mount).is_err(),
        "unknown image identity cannot prove absence"
    );
    assert_eq!(
        inventory_owner(&json!({"images":[]}), image, mount).unwrap(),
        None
    );
    for bad in [
        "/dev/disk",
        "/dev/disk17s1",
        "/dev/disk17/other",
        "/dev/rdisk17",
    ] {
        assert!(!device(bad, true));
    }
}
#[test]
fn native_volume_uuid_and_enospc_classification_are_exact() {
    let binding = OwnedMount {
        device: "/dev/disk17".into(),
        volume: "/dev/disk18s1".into(),
    };
    let mount = Path::new("/private/owned-volume");
    let info = json!({"DeviceNode":binding.volume,"MountPoint":mount,"FilesystemType":"apfs","VolumeUUID":"owned-volume-uuid"});
    assert!(check_volume(&info, &binding, mount, Some("owned-volume-uuid")).is_ok());
    assert!(
        check_volume(&info, &binding, mount, Some("reused-device-other-uuid")).is_err(),
        "same device number cannot transfer image ownership"
    );
    assert!(enospc(&std::io::Error::from_raw_os_error(libc::ENOSPC)));
    assert!(
        !enospc(&std::io::Error::from_raw_os_error(libc::EIO)),
        "generic IO cannot qualify kernel ENOSPC"
    );
}

fn capture_actual(
    f: &mut Fixture,
    repo: &Path,
    base: &archive::CommitOid,
    result: &archive::CommitOid,
    name: &str,
) -> capture::CaptureReceipt {
    match capture::capture(
        &mut f.store,
        &capture::CaptureRequest {
            operation_id: capture::OperationId::parse(name).unwrap(),
            repository: repo.into(),
            base: base.clone(),
            result: result.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        },
    )
    .unwrap()
    {
        capture::CaptureOutcome::Acknowledged(r) => r,
        other => panic!("native source capture not acknowledged: {other:?}"),
    }
}

fn execution_spec(f: &Fixture) -> snapshot::ExecutionSpec {
    let digest = |c: char| format!("sha256:{}", c.to_string().repeat(64));
    let limits = snapshot::ResourceLimits {
        timeout_ms: 5000,
        memory_bytes: 64 * 1024 * 1024,
        output_bytes: 1024 * 1024,
        pids: 32,
    };
    let command = snapshot::CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "true".into()],
        environment: Default::default(),
    };
    snapshot::ExecutionSpec {
        candidate_source: f.a.result.snapshot_id.to_string(),
        preparation: None,
        dependencies: None,
        configuration: None,
        harness: f.b.result.snapshot_id.to_string(),
        dataset: None,
        assertion_inventory: digest('1'),
        materialization_profile: snapshot::MATERIALIZATION_PROFILE.into(),
        profile: snapshot::LinuxProfile {
            build_image: digest('2'),
            verifier_image: digest('3'),
            architecture: "arm64".into(),
            runtime_profile: digest('4'),
            build_limits: limits,
            verifier_limits: limits,
        },
        build: command.clone(),
        verify: command,
    }
}

fn storage_capture_error(error: &capture::CaptureError) -> bool {
    match error {
        capture::CaptureError::InsufficientSpace { .. } => true,
        capture::CaptureError::Io { source, .. } => enospc(source),
        capture::CaptureError::Sqlite(rusqlite::Error::SqliteFailure(e, _)) => {
            e.code == rusqlite::ErrorCode::DiskFull
        }
        capture::CaptureError::Failed { code, detail, .. } => {
            code == "io" && detail.contains("os error 28")
        }
        _ => false,
    }
}
fn storage_snapshot_error(error: &snapshot::SnapshotError) -> bool {
    match error {
        snapshot::SnapshotError::Io(e) => enospc(e),
        snapshot::SnapshotError::Archive(archive::ArchiveError::Io { source, .. }) => {
            enospc(source)
        }
        _ => false,
    }
}

fn run_pressure(disk: &mut OwnedImage) {
    disk.attach();
    let mut f = Fixture::new();
    let repo = std::mem::replace(&mut f._repo, tempfile::tempdir().unwrap()).keep();
    let host = std::mem::replace(&mut f._host, tempfile::tempdir().unwrap()).keep();
    disk.note(
        "source-roots",
        json!({"repository":repo,"original_fixture_state":host}),
    );
    disk.assert_mounted();
    let project = f.store.project().clone();
    f.store = CollaborationStore::open(&CollaborationRoot::under_host_state(&disk.mount), &project)
        .unwrap();
    let base = f.a.base.commit.clone();
    git(&repo, &["switch", "--detach", f.a.result.commit.as_str()]);
    // An independent byte oracle and a payload too large for residual metadata
    // space prevent a tiny successful directory write from masking exhaustion.
    let payload = vec![0x2b; 1024 * 1024];
    std::fs::write(repo.join("pressure-bytes.bin"), &payload).unwrap();
    git(&repo, &["add", "pressure-bytes.bin"]);
    git(&repo, &["commit", "-qm", "source pressure payload"]);
    let result = archive::CommitOid::parse(&git(&repo, &["rev-parse", "HEAD"])).unwrap();
    let first = capture_actual(&mut f, &repo, &base, &result, "apfs-retained-a");
    f.a = archive::retained_contribution(&f.store, &first.contribution).unwrap();
    let b_base = f.b.base.commit.clone();
    let b_result = f.b.result.commit.clone();
    let second = capture_actual(&mut f, &repo, &b_base, &b_result, "apfs-retained-b");
    f.b = archive::retained_contribution(&f.store, &second.contribution).unwrap();
    let before = disk.root.join("source-before");
    archive::reconstruct(&f.store, &first.result, &before).unwrap();
    assert_eq!(
        std::fs::read(before.join("pressure-bytes.bin")).unwrap(),
        payload
    );
    assert_eq!(std::fs::read(before.join("one.txt")).unwrap(), b"A\n");
    let owner =
        target::acquire_controller(&mut f.store, "apfs-pressure", "appointed-test-controller")
            .unwrap();
    let specification = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, specification).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let descriptor = execution_spec(&f);
    let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, descriptor).unwrap();
    let orphan =
        archive::put_object(&f.store, b"unrooted independent source-pressure object").unwrap();
    age_objects(&f.store);
    std::fs::write(repo.join("new-source.bin"), vec![0x71; 1024 * 1024]).unwrap();
    git(&repo, &["add", "new-source.bin"]);
    git(&repo, &["commit", "-qm", "new source during pressure"]);
    let fresh = archive::CommitOid::parse(&git(&repo, &["rev-parse", "HEAD"])).unwrap();
    let operation = capture::OperationId::parse("apfs-full-new-capture").unwrap();
    drop(capture::hold_operation_lock(&f.store, &operation).unwrap());
    let request = capture::CaptureRequest {
        operation_id: operation.clone(),
        repository: repo.clone(),
        base: base.clone(),
        result: fresh,
        policy: capture::CapturePolicy::Required,
        retention: capture::RetentionBoundary::UntilReleased,
    };
    let fresh_object = vec![0x72; 2 * 1024 * 1024];
    let fresh_digest = ObjectDigest::of(&fresh_object);
    let fresh_path = archive::object_path(&f.store, &fresh_digest);
    assert!(
        !fresh_path.exists(),
        "fresh object must not reuse an existing CAS publication"
    );
    std::fs::create_dir_all(fresh_path.parent().unwrap()).unwrap();
    disk.assert_mounted();
    let pressure_path = disk.mount.join("owned-pressure.bin");
    let mut pressure = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&pressure_path)
        .unwrap();
    assert_eq!(Some(pressure.metadata().unwrap().dev()), disk.volume_device);
    let block = vec![0x6d; 64 * 1024];
    let mut written = 0u64;
    let error = loop {
        assert!(
            written <= IMAGE_BYTES,
            "pressure exceeded logical image capacity without ENOSPC"
        );
        match pressure.write_all(&block) {
            Ok(()) => written += block.len() as u64,
            Err(e) => break e,
        }
    };
    assert!(
        enospc(&error),
        "pressure must observe actual native kernel ENOSPC: {error}"
    );
    if let Err(e) = pressure.sync_all() {
        assert!(enospc(&e), "unexpected pressure sync error: {e}");
    }
    disk.note("kernel-enospc",json!({"raw_os_error":error.raw_os_error(),"completed_block_bytes":written,"pressure_file_bytes":pressure.metadata().unwrap().len()}));
    disk.assert_mounted();
    let capture_error = capture::capture(&mut f.store, &request)
        .expect_err("full filesystem cannot acknowledge new capture");
    disk.note(
        "capture-failure",
        json!({"code":capture_error.code(),"error":capture_error.to_string()}),
    );
    assert!(
        storage_capture_error(&capture_error),
        "capture failure must be actual storage/admission: {capture_error}"
    );
    assert!(
        capture::receipt(&mut f.store, &operation)
            .unwrap()
            .is_none(),
        "full capture cannot publish a receipt"
    );
    disk.assert_mounted();
    let publication = archive::put_object(&f.store, &fresh_object)
        .expect_err("full filesystem cannot publish fresh object");
    assert!(
        matches!(&publication,archive::ArchiveError::Io{source,..} if enospc(source)),
        "object publication must observe actual ENOSPC: {publication}"
    );
    assert!(
        !fresh_path.exists(),
        "failed fresh publication cannot leave a valid object name"
    );
    disk.note("publication-failure",json!({"code":publication.code(),"error":publication.to_string(),"object_exists":fresh_path.exists()}));
    disk.assert_mounted();
    let materialization =
        match snapshot::materialize_in(&f.store, &owner, &execution, Some(&disk.mount)) {
            Ok(_) => panic!("full filesystem cannot yield usable materialized inputs"),
            Err(e) => e,
        };
    assert!(
        storage_snapshot_error(&materialization),
        "materialization must observe actual ENOSPC: {materialization}"
    );
    disk.note("materialization-failure",json!({"error":materialization.to_string(),"usable_inputs_returned":false,"artifact_created":false}));
    let after = disk.root.join("source-during-pressure");
    archive::reconstruct(&f.store, &first.result, &after).unwrap();
    assert_eq!(
        std::fs::read(after.join("pressure-bytes.bin")).unwrap(),
        payload,
        "earlier retained source must survive physical pressure"
    );
    assert_eq!(
        capture::receipt(
            &mut f.store,
            &capture::OperationId::parse("apfs-retained-a").unwrap()
        )
        .unwrap(),
        Some(first.clone())
    );
    disk.assert_mounted();
    pressure.set_len(0).unwrap();
    pressure.sync_all().unwrap();
    drop(pressure);
    std::fs::remove_file(&pressure_path).unwrap();
    let recovered = capture::capture(&mut f.store, &request).unwrap();
    assert!(
        matches!(recovered, capture::CaptureOutcome::Acknowledged(_)),
        "valid capture must recover after owned capacity release"
    );
    let receipt = capture::receipt(&mut f.store, &operation).unwrap().unwrap();
    assert_eq!(
        capture::capture(&mut f.store, &request).unwrap(),
        capture::CaptureOutcome::Acknowledged(receipt)
    );
    for _ in 0..2 {
        let recovery = capture::recover(&mut f.store).unwrap();
        assert!(recovery.errors.is_empty());
        assert!(recovery.recovered.is_empty());
    }
    disk.assert_mounted();
    let materialized =
        snapshot::materialize_in(&f.store, &owner, &execution, Some(&disk.mount)).unwrap();
    assert_eq!(
        std::fs::read(materialized.source_dir().join("pressure-bytes.bin")).unwrap(),
        payload,
        "restored materialization must preserve exact original source"
    );
    drop(materialized);
    let options = crate::collaboration_gc::GcOptions::default();
    let plan = crate::collaboration_gc::plan(&mut f.store, &options).unwrap();
    let object_path = archive::object_path(&f.store, &orphan);
    let project_directory = archive::objects_dir(&f.store)
        .parent()
        .unwrap()
        .to_path_buf();
    assert!(
        plan.reclaimable
            .iter()
            .any(|e| project_directory.join(&e.relpath) == object_path)
    );
    crate::collaboration_gc::apply(&mut f.store, &plan.digest, &options).unwrap();
    assert!(!archive::object_path(&f.store, &orphan).exists());
    let final_source = disk.root.join("source-after-gc");
    archive::reconstruct(&f.store, &first.result, &final_source).unwrap();
    assert_eq!(
        std::fs::read(final_source.join("pressure-bytes.bin")).unwrap(),
        payload,
        "retained source must survive recovery GC"
    );
    assert_eq!(
        snapshot::load(&f.store, execution.reference()).unwrap(),
        execution
    );
    let residual = std::fs::read_dir(&disk.mount)
        .unwrap()
        .map(|entry| entry.expect("inspect every materialization residue"))
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("aethyme-execution-")
        })
        .count();
    assert_eq!(
        residual, 0,
        "failed materialization cannot leave unbounded temporary roots"
    );
    disk.note("source-profile-positive",json!({"retained_source":first.result.to_string(),"recovery_idempotent":true,"gc_orphan_removed":true,"materialization_residual_roots":residual,"native_build_or_verifier_claimed":false}));
    drop(owner);
    drop(f);
}

#[test]
#[ignore = "one owned 512 MiB native macOS APFS image; explicit profile required; no host/daemon disk fill"]
fn actual_native_source_capture_and_materialization_preserve_inputs_under_enospc() {
    assert_eq!(
        std::env::var("AETHYME_NATIVE_SOURCE_PRESSURE_PROFILE").as_deref(),
        Ok(PROFILE),
        "explicit owned filesystem profile required"
    );
    let mut disk = OwnedImage::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_pressure(&mut disk)));
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| disk.cleanup()));
    disk.note("attempt-result",json!({"profile_passed":result.is_ok(),"cleanup_confirmed":matches!(&cleanup,Ok(Ok(()))),"image_preserved":true}));
    assert!(
        matches!(&cleanup, Ok(Ok(()))),
        "owned image cleanup must be confirmed; preserved {}",
        disk.root.display()
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic)
    }
    assert!(disk.cleanup_confirmed);
}
