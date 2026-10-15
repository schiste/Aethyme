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
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

const IMAGE_BYTES: u64 = 512 * 1024 * 1024;
const HOST_HEADROOM: u64 = 2 * 1024 * 1024 * 1024;
const PROFILE: &str = "owned-apfs-source-pressure/provisional-v0";
const JOURNAL_HEADROOM_BYTES: u64 = 16 * 1024 * 1024;
const PRESSURE_BLOB_BYTES: usize = 32 * 1024 * 1024;
const PRESSURE_BLOCK_BYTES: usize = 64 * 1024;

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

fn host_capacity_admitted(available: u64, remaining: u64) -> bool {
    available >= HOST_HEADROOM.saturating_add(remaining)
}

fn backing_available(root: &Path) -> std::io::Result<u64> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)?;
    // SAFETY: integer-only statvfs is zero-initializable; the live descriptor
    // and writable output remain valid throughout this synchronous call.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: directory owns a live descriptor and stat is a valid writable
    // output pointer for the duration of this synchronous native call.
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(u64::from(stat.f_bavail).saturating_mul(stat.f_frsize))
}

fn independent_absence(
    value: &Value,
    binding: Option<&OwnedMount>,
    mounted: bool,
) -> Result<(), String> {
    if mounted {
        return Err("private mount remains on a different filesystem".into());
    }
    let devices = value
        .get("AllDisks")
        .and_then(Value::as_array)
        .ok_or("unknown native device inventory cannot prove absence")?;
    for entry in devices {
        let identifier = entry.as_str().ok_or("unknown native device identity")?;
        if !identifier.strip_prefix("disk").is_some_and(|rest| {
            let parts: Vec<_> = rest.split('s').collect();
            !parts.is_empty()
                && parts.len() <= 4
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()))
        }) {
            return Err("invalid native device inventory identity".into());
        }
        if binding.is_some_and(|b| {
            identifier == b.device.trim_start_matches("/dev/")
                || identifier == b.volume.trim_start_matches("/dev/")
        }) {
            return Err("owned native device remains present".into());
        }
    }
    Ok(())
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
    attachment_attempted: bool,
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
            attachment_attempted: false,
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
    fn assert_backing_capacity(&self, remaining: u64) {
        let available = backing_available(&self.root).expect("observe host backing space");
        assert!(
            host_capacity_admitted(available, remaining),
            "host backing headroom admission failed; pressure is unqualified"
        );
    }
    fn confirm_absence(&mut self) -> Result<(), String> {
        if self.attachment_attempted && self.binding.is_none() {
            return Err(
                "attachment lacks confirmed ownership; operator reconciliation required".into(),
            );
        }
        let host_device = std::fs::metadata(&self.root)
            .map_err(|e| e.to_string())?
            .dev();
        let mounted = match std::fs::symlink_metadata(&self.mount) {
            Ok(m) => {
                if !m.is_dir() || m.file_type().is_symlink() {
                    return Err("private mount path changed".into());
                }
                m.dev() != host_device
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.to_string()),
        };
        let devices = self.plist("/usr/sbin/diskutil", &["list", "-plist"])?;
        independent_absence(&devices, self.binding.as_ref(), mounted)
    }
    fn attach(&mut self) {
        self.assert_backing_capacity(IMAGE_BYTES);
        self.note("host-admission", json!({"available_bytes":backing_available(&self.root).unwrap(),"minimum_headroom_bytes":HOST_HEADROOM,"maximum_backing_bytes":IMAGE_BYTES}));

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
                    "-fs",
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
        self.assert_backing_capacity(IMAGE_BYTES);
        self.attachment_attempted = true;
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
        self.confirm_absence()?;
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

fn pressure_entry_matches(entry: &std::fs::Metadata, held: &std::fs::Metadata) -> bool {
    entry.is_file()
        && !entry.file_type().is_symlink()
        && (entry.dev(), entry.ino()) == (held.dev(), held.ino())
}

#[test]
fn replacement_pressure_entry_cannot_authorize_capacity_release() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-pressure");
    let held = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let metadata = held.metadata().unwrap();
    assert!(pressure_entry_matches(
        &std::fs::symlink_metadata(&path).unwrap(),
        &metadata
    ));
    std::fs::rename(&path, root.path().join("pinned-original")).unwrap();
    std::fs::write(&path, b"unrelated replacement").unwrap();
    assert!(
        !pressure_entry_matches(&std::fs::symlink_metadata(&path).unwrap(), &metadata),
        "replaced pressure path cannot authorize capacity release"
    );
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(root.path().join("pinned-original"), &path).unwrap();
    assert!(
        !pressure_entry_matches(&std::fs::symlink_metadata(&path).unwrap(), &metadata),
        "symlink pressure path cannot authorize capacity release"
    );
}

fn enospc(error: &std::io::Error) -> bool {
    // tempfile preserves ErrorKind while its private path wrapper hides the
    // raw errno and exposes no underlying raw-OS cause. Keep the typed kind;
    // never classify storage failure by matching its display text.
    error.raw_os_error() == Some(libc::ENOSPC) || error.kind() == std::io::ErrorKind::StorageFull
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
    assert!(host_capacity_admitted(
        HOST_HEADROOM + IMAGE_BYTES,
        IMAGE_BYTES
    ));
    assert!(
        !host_capacity_admitted(HOST_HEADROOM + IMAGE_BYTES - 1, IMAGE_BYTES),
        "insufficient backing capacity must refuse"
    );
    assert!(
        !host_capacity_admitted(HOST_HEADROOM - 1, 0),
        "host headroom must remain preserved"
    );
    let absent = json!({"AllDisks":["disk0", "disk0s1", "disk0s1s1"]});
    assert!(independent_absence(&absent, None, false).is_ok());
    assert!(
        independent_absence(&absent, None, true).is_err(),
        "inventory absence cannot settle a live mount"
    );
    assert!(independent_absence(&json!({}), None, false).is_err());

    let binding = OwnedMount {
        device: "/dev/disk17".into(),
        volume: "/dev/disk18s1".into(),
    };
    assert!(
        independent_absence(&json!({"AllDisks":["disk17"]}), Some(&binding), false).is_err(),
        "owned device presence cannot settle cleanup"
    );
    let mount = Path::new("/private/owned-volume");
    let info = json!({"DeviceNode":binding.volume,"MountPoint":mount,"FilesystemType":"apfs","VolumeUUID":"owned-volume-uuid"});
    assert!(check_volume(&info, &binding, mount, Some("owned-volume-uuid")).is_ok());
    assert!(
        check_volume(&info, &binding, mount, Some("reused-device-other-uuid")).is_err(),
        "same device number cannot transfer image ownership"
    );
    assert!(enospc(&std::io::Error::from_raw_os_error(libc::ENOSPC)));
    assert!(
        enospc(&std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "wrapped path context"
        )),
        "typed wrapped storage-full must remain classified"
    );
    assert!(
        !enospc(&std::io::Error::other(
            "No space left on device (os error 28)"
        )),
        "display text cannot qualify storage failure"
    );
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

fn volume_available(path: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: statvfs writes a plain integer struct and receives a valid path.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) }, 0);
    #[allow(clippy::unnecessary_cast)]
    (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)
}

fn deterministic_payload(bytes: usize, mut state: u64) -> Vec<u8> {
    (0..bytes)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

struct OwnedPressure {
    path: PathBuf,
    file: File,
    device: u64,
    inode: u64,
}

fn fill_to_headroom(disk: &mut OwnedImage, label: &str) -> OwnedPressure {
    disk.assert_mounted();
    let path = disk.mount.join(format!("owned-{label}-pressure.bin"));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .unwrap();
    let owner = file.metadata().unwrap();
    assert_eq!(Some(owner.dev()), disk.volume_device);
    let block = deterministic_payload(PRESSURE_BLOCK_BYTES, 0x4f5a_2913_78bc_dfe1);
    let mut written = 0u64;
    loop {
        let available = volume_available(&disk.mount);
        if available <= JOURNAL_HEADROOM_BYTES + block.len() as u64 {
            break;
        }
        disk.assert_backing_capacity(IMAGE_BYTES.saturating_sub(written));
        file.write_all(&block).unwrap();
        written += block.len() as u64;
    }
    file.sync_all().unwrap();
    let available = volume_available(&disk.mount);
    assert!(available >= JOURNAL_HEADROOM_BYTES.saturating_sub(block.len() as u64));
    assert!(available <= JOURNAL_HEADROOM_BYTES + 2 * block.len() as u64);
    assert!(available < PRESSURE_BLOB_BYTES as u64);
    let current = file.metadata().unwrap();
    assert_eq!(Some(current.dev()), disk.volume_device);
    assert!(current.len() <= IMAGE_BYTES);
    disk.note(
        "bounded-pressure-held",
        json!({"label":label,"pressure_path":path,"pressure_file_bytes":current.len(),"remaining_volume_bytes":available,"journal_headroom_bytes":JOURNAL_HEADROOM_BYTES,"source_blob_bytes":PRESSURE_BLOB_BYTES,"volume_device":current.dev(),"volume_inode":current.ino()}),
    );
    OwnedPressure {
        path,
        file,
        device: current.dev(),
        inode: current.ino(),
    }
}

fn release_pressure(disk: &mut OwnedImage, pressure: OwnedPressure, label: &str) {
    let opened = pressure.file.metadata().unwrap();
    let entry = std::fs::symlink_metadata(&pressure.path).unwrap();
    assert!(pressure_entry_matches(&entry, &opened));
    assert_eq!(opened.dev(), pressure.device);
    assert_eq!(opened.ino(), pressure.inode);
    std::fs::remove_file(&pressure.path).unwrap();
    drop(pressure.file);
    File::open(&disk.mount).unwrap().sync_all().unwrap();
    disk.assert_mounted();
    disk.note(
        "bounded-pressure-released",
        json!({"label":label,"only_owned_entry_unlinked":true,"owned_descriptor_closed":true,"previous_inode":pressure.inode,"previous_bytes":opened.len(),"remaining_volume_bytes":volume_available(&disk.mount)}),
    );
}

#[derive(Debug)]
enum InterleaveEvent {
    CaptureAtLargeBlob { commit: String, bytes: u64 },
    CaptureIoError { errno: Option<i32>, error: String },
    HydrationBeforeSource { path: PathBuf },
    HydrationIoError { errno: Option<i32>, error: String },
}

struct InterleaveGate {
    event: mpsc::Sender<InterleaveEvent>,
    released: (Mutex<bool>, Condvar),
}

impl InterleaveGate {
    fn pause(&self, event: InterleaveEvent) {
        self.event.send(event).unwrap();
        let (lock, ready) = &self.released;
        let mut released = lock.lock().unwrap();
        while !*released {
            released = ready.wait(released).unwrap();
        }
    }

    fn release(&self) {
        let (lock, ready) = &self.released;
        *lock.lock().unwrap() = true;
        ready.notify_all();
    }
}

struct ReleaseGate(Arc<InterleaveGate>);

impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn interleave_event(receiver: &mpsc::Receiver<InterleaveEvent>) -> InterleaveEvent {
    receiver
        .recv_timeout(Duration::from_secs(60))
        .expect("native I/O operation did not reach its ordered pressure gate")
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
    f.store = CollaborationStore::open(
        &CollaborationRoot::under_host_state(&disk.mount),
        &project,
        &[],
    )
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
    f.a = archive::retained_contribution(&f.store, &first.contribution)
        .unwrap()
        .expect("acknowledged source retained");
    let b_base = f.b.base.commit.clone();
    let b_result = f.b.result.commit.clone();
    let second = capture_actual(&mut f, &repo, &b_base, &b_result, "apfs-retained-b");
    f.b = archive::retained_contribution(&f.store, &second.contribution)
        .unwrap()
        .expect("acknowledged source retained");
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
        disk.assert_backing_capacity(IMAGE_BYTES.saturating_sub(written));
        match pressure.write_all(&block) {
            Ok(()) => written += block.len() as u64,
            Err(e) => break e,
        }
    };
    assert!(
        error.raw_os_error() == Some(libc::ENOSPC),
        "pressure must observe actual native kernel ENOSPC: {error}"
    );
    if let Err(e) = pressure.sync_all() {
        assert!(enospc(&e), "unexpected pressure sync error: {e}");
    }
    disk.note("kernel-enospc",json!({"raw_os_error":error.raw_os_error(),"completed_block_bytes":written,"pressure_file_bytes":pressure.metadata().unwrap().len()}));
    disk.assert_backing_capacity(0);
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
    // A failed APFS write may also make ftruncate fail. Unlink the exact
    // create_new owned entry while its descriptor pins its inode, then close
    // the last owned descriptor to release its blocks. Never truncate or
    // remove a replaced path merely because it has the same spelling.
    let owned = pressure.metadata().unwrap();
    let entry = std::fs::symlink_metadata(&pressure_path).unwrap();
    assert!(
        pressure_entry_matches(&entry, &owned),
        "capacity release must retain exact pressure ownership"
    );
    std::fs::remove_file(&pressure_path).unwrap();
    drop(pressure);
    File::open(&disk.mount).unwrap().sync_all().unwrap();
    disk.note("owned-pressure-released",json!({"only_owned_entry_unlinked":true,"owned_descriptor_closed":true,"previous_inode":owned.ino(),"previous_bytes":owned.len()}));
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
    let project_directory = f.store.project_dir();
    assert!(
        plan.reclaimable
            .iter()
            .any(|e| project_directory.join(&e.relpath) == object_path),
        "actual GC must select independently published orphan"
    );
    crate::collaboration_gc::apply(&mut f.store, &plan.digest, &options).unwrap();
    assert!(
        !archive::object_path(&f.store, &orphan).exists(),
        "actual GC must remove the selected orphan"
    );
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

fn t45_gc_plan(
    store: &CollaborationStore,
    collector: &mut CollaborationStore,
    options: &crate::collaboration_gc::GcOptions,
    label: &str,
) -> (ObjectDigest, crate::collaboration_gc::GcPlan) {
    let orphan =
        archive::put_object(store, format!("independent T45 {label} orphan").as_bytes()).unwrap();
    age_objects(store);
    let plan = crate::collaboration_gc::plan(collector, options).unwrap();
    let orphan_path = archive::object_path(collector, &orphan);
    assert!(
        plan.reclaimable
            .iter()
            .any(|entry| collector.project_dir().join(&entry.relpath) == orphan_path),
        "the pre-recorded GC plan must select the named {label} orphan"
    );
    (orphan, plan)
}

fn apply_after_pressure(
    collector: &mut CollaborationStore,
    orphan: &ObjectDigest,
    plan: &crate::collaboration_gc::GcPlan,
    options: &crate::collaboration_gc::GcOptions,
) {
    let path = archive::object_path(collector, orphan);
    let report = crate::collaboration_gc::apply(collector, &plan.digest, options).unwrap();
    assert!(
        report
            .reclaimed
            .iter()
            .any(|entry| collector.project_dir().join(&entry.relpath) == path),
        "successful post-pressure GC must reclaim the exact named orphan"
    );
    assert!(!path.exists());
}

fn verify_retained_ab(
    store: &CollaborationStore,
    disk: &OwnedImage,
    first: &archive::RetainedContribution,
    second: &archive::RetainedContribution,
    payload: &[u8],
    phase: &str,
) {
    let restored_a = disk.root.join(format!("retained-a-{phase}-after-t45-gc"));
    let restored_b = disk.root.join(format!("retained-b-{phase}-after-t45-gc"));
    assert_eq!(
        archive::reconstruct(store, &first.result.snapshot_id, &restored_a)
            .unwrap()
            .id(),
        first.result.snapshot_id
    );
    assert_eq!(
        std::fs::read(restored_a.join("pressure-bytes.bin")).unwrap(),
        payload,
        "retained A bytes must remain exact after pressure and GC"
    );
    assert_eq!(std::fs::read(restored_a.join("one.txt")).unwrap(), b"A\n");
    assert_eq!(
        archive::reconstruct(store, &second.result.snapshot_id, &restored_b)
            .unwrap()
            .id(),
        second.result.snapshot_id
    );
    assert_eq!(
        std::fs::read(restored_b.join("one.txt")).unwrap(),
        b"base\n"
    );
    assert_eq!(std::fs::read(restored_b.join("two.txt")).unwrap(), b"B\n");
}

fn run_t45_capture_hydration_interleavings(disk: &mut OwnedImage) {
    disk.attach();
    let mut fixture = Fixture::new();
    let repo = std::mem::replace(&mut fixture._repo, tempfile::tempdir().unwrap()).keep();
    let host = std::mem::replace(&mut fixture._host, tempfile::tempdir().unwrap()).keep();
    disk.note(
        "source-roots",
        json!({"repository":repo,"original_fixture_state":host}),
    );
    disk.assert_mounted();
    let project = fixture.store.project().clone();
    let root = CollaborationRoot::under_host_state(&disk.mount);
    fixture.store = CollaborationStore::open(&root, &project, &[]).unwrap();

    // Retain one large source so the later hydration performs a substantial
    // real write. This baseline capture is not the injected-failure case.
    let baseline = fixture.a.base.commit.clone();
    git(
        &repo,
        &["switch", "--detach", fixture.a.result.commit.as_str()],
    );
    let retained_payload = deterministic_payload(PRESSURE_BLOB_BYTES, 0x2831_6d9a_4c7e_b5f0);
    std::fs::write(repo.join("pressure-bytes.bin"), &retained_payload).unwrap();
    git(&repo, &["add", "pressure-bytes.bin"]);
    git(&repo, &["commit", "-qm", "T45 retained source payload"]);
    let retained_commit = archive::CommitOid::parse(&git(&repo, &["rev-parse", "HEAD"])).unwrap();
    let first_receipt = capture_actual(
        &mut fixture,
        &repo,
        &baseline,
        &retained_commit,
        "t45-retained-a",
    );
    fixture.a = archive::retained_contribution(&fixture.store, &first_receipt.contribution)
        .unwrap()
        .expect("baseline A source is retained");
    let b_base = fixture.b.base.commit.clone();
    let b_result = fixture.b.result.commit.clone();
    let second_receipt = capture_actual(&mut fixture, &repo, &b_base, &b_result, "t45-retained-b");
    fixture.b = archive::retained_contribution(&fixture.store, &second_receipt.contribution)
        .unwrap()
        .expect("baseline B source is retained");
    let available_before_capture = volume_available(&disk.mount);
    assert!(
        available_before_capture
            > crate::collaboration_capture::RESERVATION_FLOOR_BYTES
                + (2 * PRESSURE_BLOB_BYTES) as u64
                + JOURNAL_HEADROOM_BYTES,
        "the real capture preflight must have enough APFS capacity to pass"
    );
    let owner = target::acquire_controller(
        &mut fixture.store,
        "t45-apfs-pressure",
        "t45-pressure-controller",
    )
    .unwrap();
    let descriptor = execution_spec(&fixture);
    let revision = target::propose_revision(&mut fixture.store, &owner, fixture.spec()).unwrap();
    target::select_revision(&mut fixture.store, &owner, &revision.id).unwrap();
    let execution = snapshot::freeze(&mut fixture.store, &owner, &revision.id, descriptor).unwrap();

    let options = crate::collaboration_gc::GcOptions::default();
    let mut collector = CollaborationStore::open(&root, &project, &[]).unwrap();
    let (capture_orphan, capture_plan) =
        t45_gc_plan(&fixture.store, &mut collector, &options, "capture");

    // Use a distinct branch from the fixture base so the large object is
    // new and cannot be satisfied by the already retained A or B objects.
    let capture_base = fixture.a.base.commit.clone();
    git(&repo, &["switch", "--detach", capture_base.as_str()]);
    let capture_payload = deterministic_payload(PRESSURE_BLOB_BYTES, 0x6a8e_7c31_b4d2_f905);
    std::fs::write(repo.join("00-capture-pressure.bin"), &capture_payload).unwrap();
    git(&repo, &["add", "00-capture-pressure.bin"]);
    git(&repo, &["commit", "-qm", "T45 new capture payload"]);
    let capture_result = archive::CommitOid::parse(&git(&repo, &["rev-parse", "HEAD"])).unwrap();
    let failed_digest = ObjectDigest::of(&capture_payload);
    assert!(!archive::has_object(&fixture.store, &failed_digest));
    let operation = capture::OperationId::parse("t45-capture-enospc").unwrap();
    let archive_temporaries_before = std::fs::read_dir(archive::spool_dir(&fixture.store))
        .unwrap()
        .map(|entry| entry.expect("inspect every pre-capture archive temporary"))
        .map(|entry| entry.file_name())
        .collect::<Vec<_>>();
    assert!(
        archive_temporaries_before.is_empty(),
        "fixture starts without archive temporaries"
    );
    let request = capture::CaptureRequest {
        operation_id: operation.clone(),
        repository: repo.clone(),
        base: capture_base,
        result: capture_result.clone(),
        policy: capture::CapturePolicy::Required,
        retention: capture::RetentionBoundary::UntilReleased,
    };
    let mut capture_store = CollaborationStore::open(&root, &project, &[]).unwrap();
    let (events, event_rx) = mpsc::channel();
    let capture_ready = Arc::new(InterleaveGate {
        event: events.clone(),
        released: (Mutex::new(false), Condvar::new()),
    });
    let capture_error = Arc::new(InterleaveGate {
        event: events.clone(),
        released: (Mutex::new(false), Condvar::new()),
    });
    let (capture_result, capture_pressure) = std::thread::scope(|scope| {
        let _release_ready = ReleaseGate(capture_ready.clone());
        let _release_error = ReleaseGate(capture_error.clone());
        let ready_gate = capture_ready.clone();
        let error_gate = capture_error.clone();
        let expected_commit = capture_result.to_string();
        let worker = scope.spawn(move || {
            let before_copy_write = Box::new(move |commit: &archive::CommitOid, bytes: u64| {
                if commit.as_str() == expected_commit && bytes == PRESSURE_BLOB_BYTES as u64 {
                    ready_gate.pause(InterleaveEvent::CaptureAtLargeBlob {
                        commit: commit.to_string(),
                        bytes,
                    });
                }
            });
            let on_copy_error = Box::new(move |error: &archive::ArchiveError| {
                if let archive::ArchiveError::Io { source, .. } = error
                    && source.raw_os_error() == Some(libc::ENOSPC)
                {
                    error_gate.pause(InterleaveEvent::CaptureIoError {
                        errno: source.raw_os_error(),
                        error: error.to_string(),
                    });
                }
            });
            let hooks = capture::Hooks {
                before_copy_write: Some(before_copy_write),
                on_copy_error: Some(on_copy_error),
                ..capture::Hooks::default()
            };
            capture::capture_with(&mut capture_store, &request, &hooks)
        });

        match interleave_event(&event_rx) {
            InterleaveEvent::CaptureAtLargeBlob { commit, bytes } => {
                assert_eq!(commit, capture_result.as_str());
                assert_eq!(bytes, PRESSURE_BLOB_BYTES as u64);
            }
            other => panic!("unexpected pre-pressure capture event {other:?}"),
        }
        assert!(
            matches!(
                crate::collaboration_gc::apply(&mut collector, &capture_plan.digest, &options),
                Err(crate::collaboration_gc::GcError::ArchiveInUse)
            ),
            "capture GC must return ArchiveInUse before pressure while capture holds archive SH"
        );
        disk.note(
            "capture-gc-before-pressure",
            json!({"archive_shared_reader_held":true,"gc_apply":"archive_in_use","capture_state":"copying","capture_preflight_passed":true}),
        );
        let pressure = fill_to_headroom(disk, "capture");
        capture_ready.release();
        match interleave_event(&event_rx) {
            InterleaveEvent::CaptureIoError { errno, error } => {
                assert_eq!(
                    errno,
                    Some(libc::ENOSPC),
                    "capture must hit raw native ENOSPC: {error}"
                );
                disk.note(
                    "capture-raw-enospc-gc-gate",
                    json!({"errno":errno,"error":error,"journal_headroom_bytes":JOURNAL_HEADROOM_BYTES,"gc_apply":"pending_at_error_gate","archive_shared_reader_held":true}),
                );
            }
            other => panic!("unexpected capture I/O event {other:?}"),
        }
        assert!(
            matches!(
                crate::collaboration_gc::apply(&mut collector, &capture_plan.digest, &options),
                Err(crate::collaboration_gc::GcError::ArchiveInUse)
            ),
            "capture GC must return ArchiveInUse inside the raw ENOSPC callback while capture holds archive SH"
        );
        capture_error.release();
        let result = worker.join().unwrap();
        (result, pressure)
    });
    let capture_error = capture_result.expect_err("real ENOSPC cannot acknowledge capture");
    assert!(
        storage_capture_error(&capture_error),
        "capture must report its native storage failure, got {capture_error}"
    );
    release_pressure(disk, capture_pressure, "capture");
    assert!(
        capture::receipt(&mut fixture.store, &operation)
            .unwrap()
            .is_none()
    );
    let recovery = capture::recover(&mut fixture.store).unwrap();
    assert!(
        recovery.errors.is_empty(),
        "capture failure recovery must be clean"
    );
    assert!(
        recovery.recovered.is_empty(),
        "failed capture cannot recover as a receipt"
    );
    let archive_temporaries_after = std::fs::read_dir(archive::spool_dir(&fixture.store))
        .unwrap()
        .map(|entry| entry.expect("inspect every post-capture archive temporary"))
        .map(|entry| entry.file_name())
        .collect::<Vec<_>>();
    assert!(
        archive_temporaries_after.is_empty(),
        "failed capture cannot leave a partial archive temporary"
    );
    let operation_state: String = fixture
        .store
        .connection()
        .query_row(
            "SELECT state FROM capture_operations WHERE operation_id = ?1",
            [operation.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(operation_state, "failed");
    let partial_result_rows: i64 = fixture
        .store
        .connection()
        .query_row(
            "SELECT count(*) FROM retained_snapshots WHERE commit_oid = ?1",
            [capture_result.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        partial_result_rows, 0,
        "failed capture cannot retain a partial result"
    );
    assert!(!archive::has_object(&fixture.store, &failed_digest));
    apply_after_pressure(&mut collector, &capture_orphan, &capture_plan, &options);
    verify_retained_ab(
        &fixture.store,
        disk,
        &fixture.a,
        &fixture.b,
        &retained_payload,
        "after-capture",
    );

    let (hydrate_orphan, hydrate_plan) =
        t45_gc_plan(&fixture.store, &mut collector, &options, "hydration");
    let mut materialize_store = CollaborationStore::open(&root, &project, &[]).unwrap();
    let (events, event_rx) = mpsc::channel();
    let hydrate_ready = Arc::new(InterleaveGate {
        event: events.clone(),
        released: (Mutex::new(false), Condvar::new()),
    });
    let hydrate_error = Arc::new(InterleaveGate {
        event: events,
        released: (Mutex::new(false), Condvar::new()),
    });
    let temporary_root = disk.mount.clone();
    let owner_ref = &owner;
    let execution_ref = &execution;
    let (materialize_result, hydrate_pressure) = std::thread::scope(|scope| {
        let _release_ready = ReleaseGate(hydrate_ready.clone());
        let _release_error = ReleaseGate(hydrate_error.clone());
        let ready_gate = hydrate_ready.clone();
        let error_gate = hydrate_error.clone();
        let worker = scope.spawn(move || {
            let mut before_reconstruct = move |role: &str, path: &Path| {
                if role == "source" {
                    std::fs::create_dir(path).unwrap();
                    ready_gate.pause(InterleaveEvent::HydrationBeforeSource {
                        path: path.to_path_buf(),
                    });
                }
            };
            let mut on_reconstruct_error = move |error: &archive::ArchiveError| {
                if let archive::ArchiveError::Io { source, .. } = error
                    && source.raw_os_error() == Some(libc::ENOSPC)
                {
                    error_gate.pause(InterleaveEvent::HydrationIoError {
                        errno: source.raw_os_error(),
                        error: error.to_string(),
                    });
                }
            };
            snapshot::materialize_in_observed(
                &materialize_store,
                owner_ref,
                execution_ref,
                Some(&temporary_root),
                &mut before_reconstruct,
                &mut on_reconstruct_error,
            )
        });

        match interleave_event(&event_rx) {
            InterleaveEvent::HydrationBeforeSource { path } => {
                assert!(path.starts_with(&disk.mount));
                assert!(path.is_dir());
                assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
            }
            other => panic!("unexpected pre-pressure hydration event {other:?}"),
        }
        assert!(
            matches!(
                crate::collaboration_gc::apply(&mut collector, &hydrate_plan.digest, &options),
                Err(crate::collaboration_gc::GcError::ArchiveInUse)
            ),
            "hydration GC must return ArchiveInUse before pressure while materialization holds archive SH"
        );
        disk.note(
            "hydration-gc-before-pressure",
            json!({"archive_shared_reader_held":true,"gc_apply":"archive_in_use","materialization_workspace_created":true}),
        );
        let pressure = fill_to_headroom(disk, "hydration");
        hydrate_ready.release();
        match interleave_event(&event_rx) {
            InterleaveEvent::HydrationIoError { errno, error } => {
                assert_eq!(
                    errno,
                    Some(libc::ENOSPC),
                    "hydration must hit raw native ENOSPC: {error}"
                );
                disk.note(
                    "hydration-raw-enospc-gc-gate",
                    json!({"errno":errno,"error":error,"journal_headroom_bytes":JOURNAL_HEADROOM_BYTES,"gc_apply":"pending_at_error_gate","archive_shared_reader_held":true}),
                );
            }
            other => panic!("unexpected hydration I/O event {other:?}"),
        }
        assert!(
            matches!(
                crate::collaboration_gc::apply(&mut collector, &hydrate_plan.digest, &options),
                Err(crate::collaboration_gc::GcError::ArchiveInUse)
            ),
            "hydration GC must return ArchiveInUse inside the raw ENOSPC callback while materialization holds archive SH"
        );
        hydrate_error.release();
        let result = worker.join().unwrap();
        (result, pressure)
    });
    let materialize_error = match materialize_result {
        Ok(_) => panic!("actual hydration ENOSPC cannot return materialized inputs"),
        Err(error) => error,
    };
    assert!(
        storage_snapshot_error(&materialize_error),
        "hydration must report its native storage failure, got {materialize_error}"
    );
    let execution_roots = std::fs::read_dir(&disk.mount)
        .unwrap()
        .map(|entry| entry.expect("inspect every T45 hydration residue"))
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("aethyme-execution-")
        })
        .count();
    assert_eq!(
        execution_roots, 0,
        "failed hydration cannot leave a temp root"
    );
    release_pressure(disk, hydrate_pressure, "hydration");
    apply_after_pressure(&mut collector, &hydrate_orphan, &hydrate_plan, &options);
    verify_retained_ab(
        &fixture.store,
        disk,
        &fixture.a,
        &fixture.b,
        &retained_payload,
        "after-hydration",
    );
    assert_eq!(
        snapshot::load(&fixture.store, execution.reference()).unwrap(),
        execution
    );
    disk.note(
        "t45-apfs-interleaving-pass",
        json!({"profile":PROFILE,"capture_preflight":"passed_real_floor","capture_fault":"raw_mid_copy_enospc","hydration_fault":"raw_reconstruction_enospc","capture_gc_contentions":2,"hydration_gc_contentions":2,"post_fault_gc":"actual_apply_reclaimed_named_orphans","capture_receipt":false,"partial_result_objects":false,"materialized_inputs_returned":false,"execution_temp_residue":execution_roots,"retained_a_exact":true,"retained_b_exact":true,"build_or_verifier_claimed":false,"host_or_daemon_pressure":false}),
    );
    drop(owner);
    drop(fixture);
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

#[test]
#[ignore = "one owned 512 MiB native macOS APFS image; real mid-copy and hydration ENOSPC with two ordered GC contentions each"]
fn actual_native_t45_capture_and_hydration_enospc_interleave_with_gc() {
    assert_eq!(
        std::env::var("AETHYME_NATIVE_SOURCE_PRESSURE_PROFILE").as_deref(),
        Ok(PROFILE),
        "explicit owned filesystem profile required"
    );
    let mut disk = OwnedImage::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_t45_capture_hydration_interleavings(&mut disk)
    }));
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| disk.cleanup()));
    disk.note(
        "attempt-result",
        json!({"profile_passed":result.is_ok(),"cleanup_confirmed":matches!(&cleanup,Ok(Ok(()))),"image_preserved":true}),
    );
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
