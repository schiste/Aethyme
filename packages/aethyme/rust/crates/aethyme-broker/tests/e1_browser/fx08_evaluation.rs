//! Separate subject library, trusted transport, and private assertion process.
use super::*;

fn compile(
    compiler: &Path,
    sysroot: &Path,
    root: &Path,
    libs: &Path,
    args: &[&str],
    label: &str,
) -> Value {
    let policy = format!(
        "(version 1)(deny default)(allow process*)(allow mach-lookup)(allow sysctl-read)(allow file-read-metadata)(allow file-map-executable)(allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/System\") (subpath \"/Library/Developer\") (subpath {}) (subpath {}) (subpath {}))(allow file-write* (subpath {}) (literal \"/dev/null\"))",
        serde_json::to_string(sysroot.to_str().unwrap()).unwrap(),
        serde_json::to_string(root.to_str().unwrap()).unwrap(),
        serde_json::to_string(libs.to_str().unwrap()).unwrap(),
        serde_json::to_string(root.to_str().unwrap()).unwrap()
    );
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", root)
        .env("TMPDIR", root)
        .current_dir(root)
        .args(["-p", &policy])
        .arg(compiler)
        .args(args);
    let result = external(&mut command, root, label, 30);
    sync_logs(&result);
    result
}

pub(super) fn evaluate(
    compiler: &Path,
    sysroot: &Path,
    root: &Path,
    source: &str,
    oracle: &[u8],
    task: &str,
) -> Value {
    let subject = root.join("subject");
    std::fs::create_dir(&subject).unwrap();
    let private = root.join("private-evaluation");
    std::fs::create_dir(&private).unwrap();
    let libs = root.join("json-runtime");
    std::fs::create_dir(&libs).unwrap();
    let cached = Path::new("/private/tmp/aethyme-l5-cargo/debug/deps");
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
    let jsonlib = libs.join("libserde_json-051464e51a9ec9d1.rlib");
    assert!(
        jsonlib.is_file(),
        "appointed exact JSON transport build missing"
    );
    let serde_arg = format!("serde_json={}", jsonlib.display());
    let dep_arg = format!("dependency={}", libs.display());
    let eval_root = monorepo().join("packages/aethyme-eval/fx08");
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
        &libs,
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
        "extern crate subject;\nfn dispatch(op:&str,a:&serde_json::Value)->serde_json::Value{{ {} }}\n{}",
        adapter["server"].as_str().unwrap(),
        std::fs::read_to_string(eval_root.join("subject-runtime.rs")).unwrap()
    );
    sync_file(&subject.join("adapter.rs"), wrapper.as_bytes());
    let adapter_compile = compile(
        compiler,
        sysroot,
        &subject,
        &libs,
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
        &libs,
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
    let child_policy = format!(
        "(version 1)(deny default)(allow mach-lookup)(allow sysctl-read)(allow file-read-metadata)(allow file-map-executable)(allow process-exec (literal {}))(allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/System\") (literal {}) (literal \"/dev/null\"))",
        serde_json::to_string(binary.to_str().unwrap()).unwrap(),
        serde_json::to_string(binary.to_str().unwrap()).unwrap()
    );
    let parent_policy = format!(
        "(version 1)(deny default)(allow process*)(allow mach-lookup)(allow sysctl-read)(allow file-read-metadata)(allow file-map-executable)(allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/System\") (subpath {}) (literal {}) (literal \"/dev/null\"))",
        serde_json::to_string(private.to_str().unwrap()).unwrap(),
        serde_json::to_string(binary.to_str().unwrap()).unwrap()
    );
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &private)
        .env("FX08_SUBJECT", &binary)
        .env("FX08_SUBJECT_POLICY", &child_policy)
        .current_dir(&private)
        .args(["-p", &parent_policy])
        .arg(private.join("oracle"))
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
    json!({"state":"completed","compile":subject_compile,"adapter_compile":adapter_compile,"oracle_compile":oracle_compile,"run":run,"checks":checks,"requested_passed":requested,"surviving_obligations_passed":survived,"passed":run["state"]=="completed" && run["exit"]==0 && requested && survived,"subject_binary_sha256":digest(&std::fs::read(binary).unwrap()),"oracle_binary_sha256":digest(&std::fs::read(private.join("oracle")).unwrap()),"adapter_sha256":digest(wrapper.as_bytes()),"proxy_sha256":digest(proxy.as_bytes()),"json_runtime":inventory,"subject_profile":"safe Rust 2024 std-only separately compiled library; private descriptor reply; no fork/network/files/unsafe access"})
}
