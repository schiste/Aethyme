//! Separate subject library, trusted transport, and private assertion process.
use super::*;

fn ancestor_reads(paths: &[&Path]) -> String {
    let mut directories = std::collections::BTreeSet::new();
    for path in paths {
        let canonical = path.canonicalize().unwrap();
        for parent in canonical.ancestors().skip(1) {
            directories.insert(parent.to_path_buf());
        }
    }
    format!(
        "(allow file-read-data {})",
        directories
            .iter()
            .map(|path| format!(
                "(literal {})",
                serde_json::to_string(path.to_str().unwrap()).unwrap()
            ))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn compile(
    compiler: &Path,
    sysroot: &Path,
    root: &Path,
    libs: &Path,
    args: &[&str],
    label: &str,
) -> Value {
    let canonical_root = root.canonicalize().unwrap();
    let root = canonical_root.as_path();
    let canonical_libs = libs.canonicalize().unwrap();
    let libs = canonical_libs.as_path();
    let mut policy = format!(
        "(version 1)(deny default)(allow process*)(allow mach-lookup)(allow sysctl-read)(allow file-read-metadata)(allow file-map-executable)(allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/System\") (subpath \"/Library/Developer\") (subpath \"/Applications/Xcode.app/Contents/Developer\") (subpath {}) (subpath {}) (subpath {}))(allow file-write* (subpath {}) (literal \"/dev/null\"))",
        serde_json::to_string(sysroot.to_str().unwrap()).unwrap(),
        serde_json::to_string(root.to_str().unwrap()).unwrap(),
        serde_json::to_string(libs.to_str().unwrap()).unwrap(),
        serde_json::to_string(root.to_str().unwrap()).unwrap()
    );
    policy.push_str(&ancestor_reads(&[root, libs, sysroot]));
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", root)
        .env("TMPDIR", root)
        .env("SDKROOT", "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk")
        .current_dir(root)
        .args(["-p", &policy])
        .arg(compiler)
        .arg("-Clinker=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang")
        .args(args);
    let result = external(&mut command, root, label, 30);
    sync_logs(&result);
    result
}

pub(super) fn freeze_runtime(libs: &Path) -> Value {
    freeze_runtime_from(libs, Path::new("/private/tmp/aethyme-l5-cargo/debug/deps"))
}
pub(super) fn freeze_runtime_from(libs: &Path, cached: &Path) -> Value {
    let appointed = cached.join("libserde_json-051464e51a9ec9d1.rlib");
    assert!(
        std::fs::symlink_metadata(&appointed).is_ok_and(|metadata| metadata.is_file()),
        "appointed JSON runtime missing before model admission"
    );
    std::fs::create_dir(libs).unwrap();
    let mut inventory = Vec::new();
    for entry in std::fs::read_dir(cached).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if name.ends_with(".rlib")
            && [
                "libserde_json-",
                "libserde_core-",
                "libserde-",
                "libitoa-",
                "libmemchr-",
                "libzmij-",
                "libindexmap-",
                "libequivalent-",
                "libhashbrown-",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.len() <= 64 * 1024 * 1024);
            sync_file(&libs.join(name), &bytes);
            inventory.push(json!({"file":name,"sha256":digest(&bytes),"bytes":bytes.len()}));
        }
    }
    let eval_root = monorepo().join("packages/aethyme-eval/fx08");
    for name in [
        "api-adapters.json",
        "proxy-runtime.rs",
        "subject-runtime.rs",
    ] {
        let bytes = std::fs::read(eval_root.join(name)).unwrap();
        assert!(bytes.len() <= 64 * 1024);
        assert!(!bytes.is_empty());
        if name == "api-adapters.json" {
            let adapters: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(adapters["schema"], "aethyme.fx08.public-api-adapters/v1");
            for task in std::iter::once("admission").chain(TASKS.iter().map(|(_, task)| *task)) {
                for field in ["server", "proxy", "signatures"] {
                    assert!(
                        adapters[task][field]
                            .as_str()
                            .is_some_and(|text| !text.is_empty()),
                        "appointed adapter contract missing before model admission"
                    );
                }
            }
        }
        sync_file(&libs.join(name), &bytes);
        inventory.push(json!({"file":name,"sha256":digest(&bytes),"bytes":bytes.len()}));
    }
    inventory.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()));
    let inventory = json!(inventory);
    sync_file(
        &libs.join("inventory.json"),
        &serde_json::to_vec_pretty(&inventory).unwrap(),
    );
    std::fs::File::open(libs).unwrap().sync_all().unwrap();
    inventory
}

pub(super) fn evaluate(
    compiler: &Path,
    sysroot: &Path,
    root: &Path,
    libs: &Path,
    source: &str,
    oracle: &[u8],
    task: &str,
) -> Value {
    let canonical_root = root.canonicalize().unwrap();
    let root = canonical_root.as_path();
    let canonical_libs = libs.canonicalize().unwrap();
    let libs = canonical_libs.as_path();
    let subject = root.join("subject");
    std::fs::create_dir(&subject).unwrap();
    let private = root.join("private-evaluation");
    std::fs::create_dir(&private).unwrap();
    let inventory: Value =
        serde_json::from_slice(&std::fs::read(libs.join("inventory.json")).unwrap()).unwrap();
    for entry in inventory.as_array().unwrap() {
        let name = entry["file"].as_str().unwrap();
        assert!(!name.contains('/') && !name.contains('\\') && !name.starts_with('.'));
        let path = libs.join(name);
        assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(digest(&bytes), entry["sha256"].as_str().unwrap());
    }
    let mut actual: Vec<String> = std::fs::read_dir(libs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    actual.sort();
    let mut expected: Vec<String> = inventory
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["file"].as_str().unwrap().to_string())
        .collect();
    expected.push("inventory.json".into());
    expected.sort();
    assert_eq!(actual, expected, "frozen dependency set must remain exact");
    let jsonlib = libs.join("libserde_json-051464e51a9ec9d1.rlib");
    assert!(
        jsonlib.is_file(),
        "appointed exact JSON transport build missing"
    );
    let serde_arg = format!("serde_json={}", jsonlib.display());
    let dep_arg = format!("dependency={}", libs.display());
    let eval_root = libs;
    let adapters: Value =
        serde_json::from_slice(&std::fs::read(eval_root.join("api-adapters.json")).unwrap())
            .unwrap();
    let adapter = &adapters[task];
    assert!(adapter["server"].is_string() && adapter["proxy"].is_string());
    sync_file(
        &subject.join("subject.rs"),
        format!("#![forbid(unsafe_code)]\n{source}").as_bytes(),
    );
    let subject_compile = compile(
        compiler,
        sysroot,
        &subject,
        libs,
        &[
            "--edition=2024",
            "--crate-type=rlib",
            "--crate-name=subject",
            "subject.rs",
            "-o",
            "libsubject.rlib",
        ],
        "subject-compile",
    );
    if subject_compile["state"] != "completed" || subject_compile["exit"] != 0 {
        return json!({"state":"subject_compile_failure","compile":subject_compile,"json_runtime":inventory});
    }
    let wrapper = format!(
        "extern crate subject;\nfn public_signatures(){{ {} }}\nfn dispatch(op:&str,a:&serde_json::Value)->serde_json::Value{{ {} }}\n{}",
        adapter["signatures"].as_str().unwrap(),
        adapter["server"].as_str().unwrap(),
        std::fs::read_to_string(eval_root.join("subject-runtime.rs")).unwrap()
    );
    sync_file(&subject.join("adapter.rs"), wrapper.as_bytes());
    let adapter_compile = compile(
        compiler,
        sysroot,
        &subject,
        libs,
        &[
            "--edition=2024",
            "adapter.rs",
            "--extern",
            "subject=libsubject.rlib",
            "--extern",
            &serde_arg,
            "-L",
            &dep_arg,
            "-o",
            "subject",
        ],
        "adapter-compile",
    );
    if adapter_compile["state"] != "completed" || adapter_compile["exit"] != 0 {
        return json!({"state":"adapter_compile_failure","compile":subject_compile,"adapter_compile":adapter_compile,"json_runtime":inventory});
    }
    let proxy = format!(
        "{}\n{}",
        std::fs::read_to_string(eval_root.join("proxy-runtime.rs")).unwrap(),
        adapter["proxy"].as_str().unwrap()
    );
    sync_file(&private.join("source.rs"), proxy.as_bytes());
    sync_file(&private.join("oracle.rs"), oracle);
    let oracle_compile = compile(
        compiler,
        sysroot,
        &private,
        libs,
        &[
            "--edition=2024",
            "--test",
            "oracle.rs",
            "--extern",
            &serde_arg,
            "-L",
            &dep_arg,
            "-o",
            "oracle",
        ],
        "oracle-compile",
    );
    if oracle_compile["state"] != "completed" || oracle_compile["exit"] != 0 {
        return json!({"state":"oracle_compile_failure","compile":subject_compile,"adapter_compile":adapter_compile,"oracle_compile":oracle_compile,"json_runtime":inventory});
    }
    let binary = subject.join("subject");
    let mut child_policy = format!(
        "(version 1)(deny default)(allow mach-lookup)(allow sysctl-read)(allow file-read-metadata)(allow file-map-executable)(allow process-exec (literal {}))(allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/System\") (literal {}) (literal \"/dev/null\"))",
        serde_json::to_string(binary.to_str().unwrap()).unwrap(),
        serde_json::to_string(binary.to_str().unwrap()).unwrap()
    );
    child_policy.push_str(&ancestor_reads(&[&binary]));
    // The independent oracle and transport contain only trusted frozen code.
    // Only the child adapter links the proposed subject library. Seatbelt is
    // applied once to that child; a host parent never executes proposed code.
    let mut command = Command::new(private.join("oracle"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &private)
        .env("FX08_SUBJECT", &binary)
        .env("FX08_SUBJECT_POLICY", &child_policy)
        .current_dir(&private)
        .args(["--nocapture", "--test-threads=1"]);
    let run = external(&mut command, root, "evaluate", 60);
    sync_logs(&run);
    let names: Vec<String> = std::str::from_utf8(oracle)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let name = line.trim().strip_prefix("fn ")?.split('(').next()?;
            (name.starts_with("requested_") || name.starts_with("survives_"))
                .then(|| name.to_string())
        })
        .collect();
    assert!(
        names.iter().any(|name| name.starts_with("requested_"))
            && names.iter().any(|name| name.starts_with("survives_")),
        "independent requested and survivor assertions required"
    );
    let bytes = std::fs::read(run["stdout"].as_str().unwrap()).unwrap();
    let stdout = String::from_utf8_lossy(&bytes);
    let checks:Vec<_>=names.iter().map(|name|json!({"name":name,"passed":stdout.lines().any(|line|line==format!("test {name} ... ok"))})).collect();
    let requested = checks
        .iter()
        .filter(|c| c["name"].as_str().unwrap().starts_with("requested_"))
        .all(|c| c["passed"] == true);
    let survived = checks
        .iter()
        .filter(|c| c["name"].as_str().unwrap().starts_with("survives_"))
        .all(|c| c["passed"] == true);
    json!({"state":"completed","compile":subject_compile,"adapter_compile":adapter_compile,"oracle_compile":oracle_compile,"run":run,"checks":checks,"requested_passed":requested,"surviving_obligations_passed":survived,"passed":run["state"]=="completed" && run["exit"]==0 && requested && survived,"subject_binary_sha256":digest(&std::fs::read(binary).unwrap()),"oracle_binary_sha256":digest(&std::fs::read(private.join("oracle")).unwrap()),"adapter_sha256":digest(wrapper.as_bytes()),"proxy_sha256":digest(proxy.as_bytes()),"json_runtime":inventory,"subject_profile":"safe Rust 2024 std-only separately compiled library; private descriptor reply; no fork/network/private-file data/unsafe access"})
}
