//! Explicit operator plans for a macOS PR-watch service. No implicit activation.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;
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

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn text(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| invalid("service paths must be UTF-8"))
}

fn xml(value: &str) -> io::Result<String> {
    if value
        .chars()
        .any(|c| c < ' ' && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(invalid("service values contain XML control characters"));
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn absolute_directory(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(invalid(
            "service directory inputs must be existing absolute directories",
        ));
    }
    path.canonicalize()
}

fn executable(path: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    if !path.is_absolute() {
        return Err(invalid("service executables must use absolute paths"));
    }
    let path = path.canonicalize()?;
    let metadata = path.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(invalid(
            "service executable is not an executable regular file",
        ));
    }
    Ok(path)
}

fn binary_digest(path: &Path) -> io::Result<String> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_BINARY_BYTES {
        return Err(invalid("service executable exceeds the identity budget"));
    }
    let mut hash = Sha256::new();
    let mut read = file.take(MAX_BINARY_BYTES + 1);
    let mut bytes = [0; 64 * 1024];
    let mut total = 0;
    loop {
        let size = read.read(&mut bytes)?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if total > MAX_BINARY_BYTES {
            return Err(invalid(
                "service executable grew beyond the identity budget",
            ));
        }
        hash.update(&bytes[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
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
        let broker = xml(text(&self.broker)?)?;
        Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{python}</string><string>-I</string><string>-B</string><string>{monitor}</string></array>
<key>EnvironmentVariables</key><dict>
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
}
