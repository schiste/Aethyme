//! Explicit operator plans for a macOS PR-watch service. No implicit activation.
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::scheduled_service::{MAX_BINARY_BYTES, binary_digest_with_hook};
use crate::scheduled_service::{
    absolute_directory, binary_digest, digest, executable, invalid, text, xml,
};

const MONITOR: &str = include_str!("../../../../scripts/adapters/aethyme-pr-monitor.py");
const ADAPTER: &str = include_str!("../../../../scripts/adapters/chau7-delivery-adapter.py");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Script {
    pub name: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    pub schema: String,
    pub repository_uuid: String,
    pub repository: PathBuf,
    pub broker: PathBuf,
    pub broker_sha256: String,
    pub python: PathBuf,
    pub python_sha256: String,
    pub home: PathBuf,
    pub interval_seconds: u64,
    pub uid: u32,
    pub label: String,
    pub directory: PathBuf,
    pub unit_path: PathBuf,
    pub scripts: Vec<Script>,
    #[serde(default)]
    pub generation: String,
    pub unit: String,
    pub digest: String,
}

pub(crate) fn script_contents() -> [(&'static str, &'static str); 2] {
    [
        ("aethyme-pr-monitor.py", MONITOR),
        ("chau7-delivery-adapter.py", ADAPTER),
    ]
}

impl Plan {
    pub(crate) fn new(
        repository: &Path,
        repository_uuid: &str,
        home: &Path,
        broker: &Path,
        python: &Path,
        interval_seconds: u64,
        uid: u32,
    ) -> io::Result<Self> {
        let valid_uuid = repository_uuid.len() == 36
            && repository_uuid.bytes().enumerate().all(|(i, c)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    c == b'-'
                } else {
                    c.is_ascii_hexdigit()
                }
            });
        if !valid_uuid || uid == 0 || !(120..=3600).contains(&interval_seconds) {
            return Err(invalid(
                "service requires a repository UUID, non-root user and interval 120..3600 seconds",
            ));
        }
        let repository = absolute_directory(repository)?;
        let home = absolute_directory(home)?;
        let broker = executable(broker)?;
        let python = executable(python)?;
        let label = format!("com.aethyme.pr-watch.{repository_uuid}");
        let directory = home
            .join("Library/Application Support/Aethyme/watch-services")
            .join(repository_uuid);
        let unit_path = home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        let scripts = script_contents()
            .into_iter()
            .map(|(name, content)| Script {
                name: name.into(),
                sha256: digest(content.as_bytes()),
            })
            .collect();
        let mut plan = Self {
            schema: "aethyme.pr-watch-service-plan/v1".into(),
            repository_uuid: repository_uuid.into(),
            repository,
            home,
            broker_sha256: binary_digest(&broker)?,
            broker,
            python_sha256: binary_digest(&python)?,
            python,
            interval_seconds,
            uid,
            label,
            directory,
            unit_path,
            scripts,
            generation: String::new(),
            unit: String::new(),
            digest: String::new(),
        };
        // Bind the unit template too, with generation and digest empty. Render
        // the final unit only after deriving this non-recursive identity.
        plan.unit = plan.unit_text()?;
        plan.generation = digest(&serde_json::to_vec(&plan).map_err(io::Error::other)?);
        plan.unit = plan.unit_text()?;
        // Empty digest is part of the versioned canonical input. Never accept
        // a deserialized plan as installation authority: recompute from inputs.
        plan.digest = digest(&serde_json::to_vec(&plan).map_err(io::Error::other)?);
        Ok(plan)
    }

    fn unit_text(&self) -> io::Result<String> {
        let python = xml(text(&self.python)?)?;
        let monitor = xml(text(&self.directory.join("aethyme-pr-monitor.py"))?)?;
        let repository = xml(text(&self.repository)?)?;
        let home = xml(text(&self.home)?)?;
        let broker = xml(text(&self.broker)?)?;
        Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{python}</string><string>-I</string><string>-B</string><string>{monitor}</string></array>
<key>EnvironmentVariables</key><dict>
<key>HOME</key><string>{home}</string>
<key>AETHYME_PR_MONITOR_REPO</key><string>{repository}</string>
<key>AETHYME_BROKER_BIN</key><string>{broker}</string>
<key>AETHYME_PR_MONITOR_BROKER_SHA256</key><string>{broker_sha256}</string>
<key>AETHYME_PR_MONITOR_GENERATION</key><string>{generation}</string>
<key>AETHYME_PR_MONITOR_RECORD_HEALTH</key><string>1</string>
<key>AETHYME_PR_MONITOR_WORKER</key><string>{label}</string>
<key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
</dict>
<key>StartInterval</key><integer>{interval}</integer>
<key>RunAtLoad</key><false/>
<key>StandardOutPath</key><string>/dev/null</string>
<key>StandardErrorPath</key><string>/dev/null</string>
</dict></plist>
"#,
            label = self.label,
            generation = self.generation,
            broker_sha256 = self.broker_sha256,
            interval = self.interval_seconds
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    const UUID: &str = "12345678-1234-4123-8123-123456789abc";

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo '& < >");
        let home = root.path().join("home");
        let broker = root.path().join("pinned broker");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(&home).unwrap();
        std::fs::write(&broker, b"synthetic executable identity").unwrap();
        std::fs::set_permissions(&broker, std::fs::Permissions::from_mode(0o700)).unwrap();
        (root, repo, home, broker)
    }

    #[test]
    fn plans_are_deterministic_escaped_and_do_not_write_or_activate() {
        let (_owner, repo, home, broker) = fixture();
        let first = Plan::new(&repo, UUID, &home, &broker, &broker, 300, 501).unwrap();
        let second = Plan::new(&repo, UUID, &home, &broker, &broker, 300, 501).unwrap();
        assert_eq!(first.digest, second.digest);
        assert_eq!(first.digest.len(), 64);
        assert_eq!(first.generation.len(), 64);
        assert!(first.unit.contains("<key>HOME</key>"));
        assert_eq!(first.generation, second.generation);
        assert_ne!(first.generation, first.digest);
        assert!(first.unit.contains(&format!(
            "<key>AETHYME_PR_MONITOR_GENERATION</key><string>{}</string>",
            first.generation
        )));
        assert!(
            first
                .unit
                .contains("<string>-I</string><string>-B</string>")
        );
        assert!(first.unit.contains("&apos;&amp; &lt; &gt;"));
        assert!(first.unit.contains("<key>RunAtLoad</key><false/>"));
        assert!(first.unit.contains("<string>/dev/null</string>"));
        assert!(!first.directory.exists());
        assert!(!first.unit_path.exists());
        assert_eq!(first.scripts.len(), 2);
    }

    #[test]
    fn shared_service_helpers_preserve_pr_watch_v1_plan_and_unit_fixture() {
        let (_owner, repo, home, broker) = fixture();
        let plan = Plan::new(&repo, UUID, &home, &broker, &broker, 300, 501).unwrap();
        assert_eq!(plan.schema, "aethyme.pr-watch-service-plan/v1");
        let serialized = serde_json::to_string(&plan).unwrap();
        assert!(serialized.starts_with(
            r#"{"schema":"aethyme.pr-watch-service-plan/v1","repository_uuid":"12345678-1234-4123-8123-123456789abc","repository":""#
        ));

        let broker_binding = format!(
            "<key>AETHYME_BROKER_BIN</key><string>{}</string>",
            xml(text(&plan.broker).unwrap()).unwrap()
        );
        assert_eq!(plan.unit.matches(&broker_binding).count(), 1);
        // The fixture deliberately uses the same executable for Python and
        // broker. Normalize this named field before replacing executable paths.
        let mut unit = plan.unit.replace(
            &broker_binding,
            "<key>AETHYME_BROKER_BIN</key><string><BROKER></string>",
        );
        for (actual, fixture) in [
            (xml(text(&plan.python).unwrap()).unwrap(), "<PYTHON>"),
            (
                xml(text(&plan.directory.join("aethyme-pr-monitor.py")).unwrap()).unwrap(),
                "<MONITOR>",
            ),
            (
                xml(text(&plan.repository).unwrap()).unwrap(),
                "<REPOSITORY>",
            ),
            (xml(text(&plan.home).unwrap()).unwrap(), "<HOME>"),
            (plan.label.clone(), "<LABEL>"),
            (plan.generation.clone(), "<GENERATION>"),
            (plan.broker_sha256.clone(), "<BROKER_SHA256>"),
        ] {
            unit = unit.replace(&actual, fixture);
        }
        assert_eq!(
            unit,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string><LABEL></string>
<key>ProgramArguments</key><array><string><PYTHON></string><string>-I</string><string>-B</string><string><MONITOR></string></array>
<key>EnvironmentVariables</key><dict>
<key>HOME</key><string><HOME></string>
<key>AETHYME_PR_MONITOR_REPO</key><string><REPOSITORY></string>
<key>AETHYME_BROKER_BIN</key><string><BROKER></string>
<key>AETHYME_PR_MONITOR_BROKER_SHA256</key><string><BROKER_SHA256></string>
<key>AETHYME_PR_MONITOR_GENERATION</key><string><GENERATION></string>
<key>AETHYME_PR_MONITOR_RECORD_HEALTH</key><string>1</string>
<key>AETHYME_PR_MONITOR_WORKER</key><string><LABEL></string>
<key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
</dict>
<key>StartInterval</key><integer>300</integer>
<key>RunAtLoad</key><false/>
<key>StandardOutPath</key><string>/dev/null</string>
<key>StandardErrorPath</key><string>/dev/null</string>
</dict></plist>
"#
        );
    }

    #[test]
    fn changing_binary_or_interval_changes_the_confirmed_plan() {
        let (_owner, repo, home, broker) = fixture();
        let a = Plan::new(&repo, UUID, &home, &broker, &broker, 300, 501).unwrap();
        let b = Plan::new(&repo, UUID, &home, &broker, &broker, 600, 501).unwrap();
        std::fs::write(&broker, b"changed identity").unwrap();
        let c = Plan::new(&repo, UUID, &home, &broker, &broker, 300, 501).unwrap();
        assert_ne!(a.digest, b.digest);
        assert_ne!(a.digest, c.digest);
        assert_ne!(a.generation, b.generation);
        assert_ne!(a.generation, c.generation);
    }

    #[test]
    fn oversized_executables_are_refused_without_reading_their_contents() {
        let (_owner, repo, home, broker) = fixture();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&broker)
            .unwrap()
            .set_len(MAX_BINARY_BYTES + 1)
            .unwrap();
        let error = Plan::new(&repo, UUID, &home, &broker, &broker, 300, 501).unwrap_err();
        assert!(error.to_string().contains("identity budget"));
    }

    #[test]
    fn paired_helpers_are_frozen_into_the_plan_with_exact_content_hashes() {
        let (_owner, repo, home, broker) = fixture();
        let plan = Plan::new(&repo, UUID, &home, &broker, &broker, 300, 501).unwrap();
        for (script, (name, content)) in plan.scripts.iter().zip(script_contents()) {
            assert_eq!(script.name, name);
            assert_eq!(script.sha256, digest(content.as_bytes()));
        }
        assert_eq!(
            plan.broker_sha256,
            "6d9d8bab02b1b20c6a5989e08f64787a3ddc44c20c1eb5417297e7464c865bd6"
        );
        assert_eq!(plan.python_sha256, plan.broker_sha256);
        assert!(!plan.unit.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn invalid_identity_interval_or_executable_is_refused() {
        let (_owner, repo, home, broker) = fixture();
        for (uuid, seconds, uid) in [
            ("../escape", 300, 501),
            (UUID, 119, 501),
            (UUID, 3601, 501),
            (UUID, 300, 0),
        ] {
            assert!(Plan::new(&repo, uuid, &home, &broker, &broker, seconds, uid).is_err());
        }
        assert!(Plan::new(&repo, UUID, &home, Path::new("relative"), &broker, 300, 501).is_err());
        std::fs::set_permissions(&broker, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Plan::new(&repo, UUID, &home, &broker, &broker, 300, 501).is_err());
    }
    #[test]
    fn identity_reader_refuses_links_and_nonregular_files_without_waiting() {
        use std::os::unix::ffi::OsStrExt as _;
        let (_owner, _repo, _home, broker) = fixture();
        let linked = broker.with_file_name("linked executable");
        std::os::unix::fs::symlink(&broker, &linked).unwrap();
        assert!(binary_digest(&linked).is_err());
        let fifo = broker.with_file_name("named pipe");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid terminated path, retained through mkfifo.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o700) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            sender.send(binary_digest(&fifo)).unwrap();
        });
        let error = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("nonregular identity input must not block opening")
            .unwrap_err();
        assert!(error.to_string().contains("regular file"));
        assert!(binary_digest(broker.parent().unwrap()).is_err());
    }

    #[test]
    fn identity_reader_rejects_named_path_replacement_after_open() {
        let (_owner, _repo, _home, broker) = fixture();
        let backup = broker.with_file_name("original executable");
        let error = binary_digest_with_hook(&broker, || {
            std::fs::rename(&broker, &backup)?;
            std::fs::write(&broker, std::fs::read(&backup)?)?;
            std::fs::set_permissions(&broker, std::fs::Permissions::from_mode(0o700))
        })
        .unwrap_err();
        assert!(error.to_string().contains("changed during identity"));
    }

    #[test]
    fn identity_reader_rejects_content_and_permission_changes_after_open() {
        let (_owner, _repo, _home, broker) = fixture();
        let error = binary_digest_with_hook(&broker, || {
            std::fs::write(&broker, b"changed and differently sized executable bytes")
        })
        .unwrap_err();
        assert!(error.to_string().contains("changed during identity"));
        let error = binary_digest_with_hook(&broker, || {
            std::fs::set_permissions(&broker, std::fs::Permissions::from_mode(0o600))
        })
        .unwrap_err();
        assert!(error.to_string().contains("changed during identity"));
    }
}
