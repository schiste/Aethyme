use std::path::PathBuf;

use super::compile::{
    CAPACITY_KEY, CompileContext, EnvironmentOwner, NAMESPACE_KEY, compile, environment_id,
    topological_order,
};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile, HostPoolSpec, Runtime};
use super::validate::{Diagnostic, validate};
use super::{Loaded, parse_loaded};
use crate::resources::{HostResourceCoordinator, HostResourceKind};

/// Fixture (a): a Python server over SQLite. Process + hooks, no containers.
const PYTHON_SQLITE: &str = r#"
schema = 1
default_profile = "backend"

[pools.envs]
limit = 4

[[profiles]]
name = "backend"
units = 1

[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "sh scripts/env/db-create.sh"
destroy = "sh scripts/env/db-drop.sh"

[[profiles.components]]
name = "api"
runtime = "process"
depends_on = ["db"]
listen = true
route = true
start = "python3 -m http.server $AETHYME_ENV_PORT_API"
health = { http = "/", timeout_seconds = 30 }
"#;

/// Fixture (b): Node behind compose with a static frontend. Different
/// runtimes, same file, no stack-specific code.
const NODE_COMPOSE: &str = r#"
schema = 1
default_profile = "static"

[engine]
default = "docker"

[pools.envs]
limit = 6

[[profiles]]
name = "static"
units = 0
uses_shared = ["baseline"]
components = [
  { name = "web", runtime = "static", build = "pnpm build", serve_dir = "dist", cache_inputs = ["apps/**", "pnpm-lock.yaml"] },
]

[[profiles]]
name = "backend"
units = 1
components = [
  { name = "api", runtime = "process", listen = true, route = true, start = "node server.js", health = { tcp = true } },
]

[[profiles]]
name = "isolated"
units = 3

[[profiles.components]]
name = "stack"
runtime = "compose"
file = "compose.env.yml"
services = ["db", "api"]

[[profiles.components]]
name = "api"
runtime = "process"
depends_on = ["stack"]
listen = true
route = true
start = "node server.js"
health = { http = "/healthz" }

[[rules]]
name = "schema"
paths = ["**/migrations/**", "db/schema/**"]
min_profile = "isolated"

[[shared]]
name = "baseline"
profile = "backend"
ref = "default_branch"
"#;

fn parse(text: &str) -> EnvironmentsFile {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    loaded.value.unwrap_or_else(|d| panic!("{}", d.render()))
}

fn diagnostics(text: &str) -> Vec<Diagnostic> {
    validate(
        &parse(text),
        &HostEnvironmentsFile::default(),
        "env.toml",
        "host.toml",
    )
}

fn context() -> CompileContext {
    CompileContext {
        repository: "repo-0123456789abcdef".into(),
        owner: EnvironmentOwner::Worktree {
            fingerprint: "f".repeat(64),
        },
        worktree_fingerprint: "f".repeat(64),
        source_commit: Some("a".repeat(40)),
        source_dirty: false,
        source_digest: "d".repeat(64),
    }
}

#[test]
fn both_fixture_stacks_validate_without_stack_specific_code() {
    for fixture in [PYTHON_SQLITE, NODE_COMPOSE] {
        let found = diagnostics(fixture);
        assert!(
            found.is_empty(),
            "{}",
            found
                .iter()
                .map(Diagnostic::render)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// One failing fixture per rule. Each must name the code and the location
/// (profile, component, field) an agent needs to fix it.
#[test]
fn every_rule_has_a_located_diagnostic() {
    struct Case {
        name: &'static str,
        toml: String,
        code: &'static str,
        profile: Option<&'static str>,
        component: Option<&'static str>,
        field: &'static str,
    }
    let profile = |components: &str| {
        format!(
            "schema = 1\n[pools.envs]\nlimit = 2\n[[profiles]]\nname = \"p\"\nunits = 1\ncomponents = [{components}]\n"
        )
    };
    let cases = vec![
        Case {
            name: "schema",
            toml: "schema = 2\n".into(),
            code: "unsupported_schema",
            profile: None,
            component: None,
            field: "schema",
        },
        Case {
            name: "cycle",
            toml: profile(
                "{ name = \"a\", runtime = \"process\", start = \"x\", depends_on = [\"b\"] },\
                 { name = \"b\", runtime = \"process\", start = \"x\", depends_on = [\"a\"] }",
            ),
            code: "depends_on_cycle",
            profile: Some("p"),
            component: Some("a"),
            field: "depends_on",
        },
        Case {
            name: "dangling dependency",
            toml: profile("{ name = \"a\", runtime = \"process\", start = \"x\", depends_on = [\"zz\"] }"),
            code: "unknown_dependency",
            profile: Some("p"),
            component: Some("a"),
            field: "depends_on",
        },
        Case {
            name: "routed without health",
            toml: profile("{ name = \"a\", runtime = \"process\", start = \"x\", listen = true, route = true }"),
            code: "route_without_health",
            profile: Some("p"),
            component: Some("a"),
            field: "health",
        },
        Case {
            name: "routed without listen",
            toml: profile(
                "{ name = \"a\", runtime = \"process\", start = \"x\", route = true, health = { command = \"true\" } }",
            ),
            code: "route_without_listen",
            profile: Some("p"),
            component: Some("a"),
            field: "route",
        },
        Case {
            name: "units over limit",
            toml: "schema = 1\n[pools.envs]\nlimit = 2\n[[profiles]]\nname = \"p\"\nunits = 3\ncomponents = [{ name = \"a\", runtime = \"process\", start = \"x\" }]\n".into(),
            code: "units_exceed_limit",
            profile: Some("p"),
            component: None,
            field: "units",
        },
        Case {
            name: "driver field mismatch",
            toml: profile("{ name = \"a\", runtime = \"process\", start = \"x\", serve_dir = \"dist\" }"),
            code: "field_not_supported",
            profile: Some("p"),
            component: Some("a"),
            field: "serve_dir",
        },
        Case {
            name: "entry path without a listener",
            toml: profile("{ name = \"a\", runtime = \"process\", start = \"x\", entry_path = \"/o/\" }"),
            code: "field_not_supported",
            profile: Some("p"),
            component: Some("a"),
            field: "entry_path",
        },
        Case {
            name: "entry path on a static component",
            toml: profile("{ name = \"a\", runtime = \"static\", serve_dir = \"dist\", entry_path = \"/o/\" }"),
            code: "field_not_supported",
            profile: Some("p"),
            component: Some("a"),
            field: "entry_path",
        },
        Case {
            name: "entry path on a hooks component",
            toml: profile("{ name = \"a\", runtime = \"hooks\", provision = \"x\", destroy = \"x\", entry_path = \"/o/\" }"),
            code: "field_not_supported",
            profile: Some("p"),
            component: Some("a"),
            field: "entry_path",
        },
        Case {
            name: "entry path without a leading slash",
            toml: profile("{ name = \"a\", runtime = \"process\", start = \"x\", entry_path = \"o/\" }"),
            code: "invalid_path",
            profile: Some("p"),
            component: Some("a"),
            field: "entry_path",
        },
        Case {

            name: "spa fallback outside static",
            toml: profile("{ name = \"a\", runtime = \"process\", start = \"x\", spa_fallback = true }"),
            code: "field_not_supported",
            profile: Some("p"),
            component: Some("a"),
            field: "spa_fallback",
        },
        Case {
            name: "process without start",
            toml: profile("{ name = \"a\", runtime = \"process\" }"),
            code: "missing_field",
            profile: Some("p"),
            component: Some("a"),
            field: "start",
        },
        Case {
            name: "compose on apple container",
            toml: profile("{ name = \"a\", runtime = \"compose\", file = \"c.yml\", engine = \"container\" }"),
            code: "engine_without_compose",
            profile: Some("p"),
            component: Some("a"),
            field: "engine",
        },
        Case {
            name: "provision without destroy",
            toml: profile("{ name = \"db\", runtime = \"hooks\", provision = \"x\" }"),
            code: "missing_destroy",
            profile: Some("p"),
            component: Some("db"),
            field: "destroy",
        },
        Case {
            name: "template without template_destroy",
            toml: profile(
                "{ name = \"db\", runtime = \"hooks\", provision = \"x\", destroy = \"y\", template = \"t\" }",
            ),
            code: "missing_template_destroy",
            profile: Some("p"),
            component: Some("db"),
            field: "template_destroy",
        },
        Case {
            name: "env collision",
            toml: profile(
                "{ name = \"a-b\", runtime = \"process\", start = \"x\" }, { name = \"a_b\", runtime = \"process\", start = \"x\" }",
            ),
            code: "component_env_collision",
            profile: Some("p"),
            component: Some("a_b"),
            field: "name",
        },
        Case {
            name: "reserved env",
            toml: profile(
                "{ name = \"a\", runtime = \"process\", start = \"x\", env = { AETHYME_ENV_ID = \"no\" } }",
            ),
            code: "reserved_env_name",
            profile: Some("p"),
            component: Some("a"),
            field: "env.AETHYME_ENV_ID",
        },
        Case {
            name: "two health kinds",
            toml: profile(
                "{ name = \"a\", runtime = \"process\", start = \"x\", listen = true, health = { tcp = true, command = \"true\" } }",
            ),
            code: "invalid_health",
            profile: Some("p"),
            component: Some("a"),
            field: "health",
        },
        Case {
            name: "duplicate rank",
            toml: "schema = 1\n[[profiles]]\nname = \"a\"\nrank = 1\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n[[profiles]]\nname = \"b\"\nrank = 1\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n".into(),
            code: "duplicate_rank",
            profile: Some("b"),
            component: None,
            field: "rank",
        },
        Case {
            name: "units without pool",
            toml: "schema = 1\n[[profiles]]\nname = \"a\"\nunits = 1\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n".into(),
            code: "missing_pool",
            profile: Some("a"),
            component: None,
            field: "pool",
        },
        Case {
            name: "repository pool without limit",
            toml: "schema = 1\n[pools.envs]\n[[profiles]]\nname = \"a\"\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n".into(),
            code: "missing_pool_limit",
            profile: None,
            component: None,
            field: "pools.envs.limit",
        },
        Case {
            name: "host pool limit set in repository",
            toml: "schema = 1\n[pools.envs]\nscope = \"host\"\nlimit = 3\n[[profiles]]\nname = \"a\"\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n".into(),
            code: "host_pool_limit_in_repository",
            profile: None,
            component: None,
            field: "pools.envs.limit",
        },
        Case {
            name: "host pool missing in host file",
            toml: "schema = 1\n[pools.envs]\nscope = \"host\"\n[[profiles]]\nname = \"a\"\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n".into(),
            code: "missing_host_pool",
            profile: None,
            component: None,
            field: "pools.envs",
        },
        Case {
            name: "rule names unknown profile",
            toml: "schema = 1\n[[profiles]]\nname = \"a\"\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n[[rules]]\nname = \"r\"\npaths = [\"x/**\"]\nmin_profile = \"nope\"\n".into(),
            code: "unknown_profile",
            profile: None,
            component: None,
            field: "rules.r.min_profile",
        },
        Case {
            name: "invalid exclude glob",
            toml: "schema = 1\n[[profiles]]\nname = \"a\"\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n[[rules]]\nname = \"r\"\npaths = [\"x/**\"]\nexclude = [\"x/[\"]\nmin_profile = \"a\"\n".into(),
            code: "invalid_glob",
            profile: None,
            component: None,
            field: "rules.r.exclude",
        },
        Case {
            name: "uses unknown shared",
            toml: "schema = 1\n[[profiles]]\nname = \"a\"\nuses_shared = [\"base\"]\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n".into(),
            code: "unknown_shared",
            profile: Some("a"),
            component: None,
            field: "uses_shared",
        },
        Case {
            name: "bad name",
            toml: "schema = 1\n[[profiles]]\nname = \"Bad\"\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n".into(),
            code: "invalid_name",
            profile: Some("Bad"),
            component: None,
            field: "name",
        },
        Case {
            name: "paths on an unrouted component",
            toml: profile(
                "{ name = \"a\", runtime = \"hooks\", provision = \"x\", destroy = \"y\", paths = [{ prefix = \"/api\", to = \"http://127.0.0.1:9\" }] }",
            ),
            code: "paths_without_route",
            profile: Some("p"),
            component: Some("a"),
            field: "paths",
        },
        Case {
            name: "relative path prefix",
            toml: profile(
                "{ name = \"site\", runtime = \"static\", serve_dir = \"dist\", paths = [{ prefix = \"api\", to = \"http://127.0.0.1:9\" }] }",
            ),
            code: "invalid_path_prefix",
            profile: Some("p"),
            component: Some("site"),
            field: "paths[0].prefix",
        },
        Case {
            name: "duplicate path prefix",
            toml: profile(
                "{ name = \"site\", runtime = \"static\", serve_dir = \"dist\", paths = [{ prefix = \"/api\", to = \"http://127.0.0.1:9\" }, { prefix = \"/api/\", to = \"http://127.0.0.1:9\" }] }",
            ),
            code: "duplicate_path_prefix",
            profile: Some("p"),
            component: Some("site"),
            field: "paths[1].prefix",
        },
        Case {
            name: "path to an unknown component",
            toml: profile(
                "{ name = \"site\", runtime = \"static\", serve_dir = \"dist\", paths = [{ prefix = \"/api\", to = \"api\" }] }",
            ),
            code: "invalid_path_target",
            profile: Some("p"),
            component: Some("site"),
            field: "paths[0].to",
        },
        Case {
            name: "path to a TLS upstream",
            toml: profile(
                "{ name = \"site\", runtime = \"static\", serve_dir = \"dist\", paths = [{ prefix = \"/api\", to = \"https://example.com\" }] }",
            ),
            code: "invalid_path_target",
            profile: Some("p"),
            component: Some("site"),
            field: "paths[0].to",
        },
        Case {
            name: "path to a shared environment the profile does not use",
            toml: profile(
                "{ name = \"site\", runtime = \"static\", serve_dir = \"dist\", paths = [{ prefix = \"/ws\", to = \"shared:realtime\" }] }",
            ),
            code: "invalid_path_target",
            profile: Some("p"),
            component: Some("site"),
            field: "paths[0].to",
        },
        Case {
            name: "path to an unrouted component",
            toml: profile(
                "{ name = \"site\", runtime = \"static\", serve_dir = \"dist\", paths = [{ prefix = \"/api\", to = \"db\" }] }, { name = \"db\", runtime = \"hooks\", provision = \"x\", destroy = \"y\" }",
            ),
            code: "invalid_path_target",
            profile: Some("p"),
            component: Some("site"),
            field: "paths[0].to",
        },
        Case {
            name: "idle too short",
            toml: "schema = 1\ndefault_idle_seconds = 5\n[[profiles]]\nname = \"a\"\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n".into(),
            code: "idle_too_short",
            profile: None,
            component: None,
            field: "default_idle_seconds",
        },
        Case {
            name: "required depends on optional",
            toml: profile(
                "{ name = \"a\", runtime = \"process\", start = \"x\", optional = true },\
                 { name = \"b\", runtime = \"process\", start = \"x\", depends_on = [\"a\"] }",
            ),
            code: "depends_on_optional",
            profile: Some("p"),
            component: Some("b"),
            field: "depends_on",
        },
        Case {
            name: "enabled_by without optional",
            toml: profile(
                "{ name = \"a\", runtime = \"process\", start = \"x\", enabled_by = [\"web/**\"] }",
            ),
            code: "enabled_by_without_optional",
            profile: Some("p"),
            component: Some("a"),
            field: "enabled_by",
        },
    ];
    for case in cases {
        let found = diagnostics(&case.toml);
        let hit = found
            .iter()
            .find(|d| d.code == case.code)
            .unwrap_or_else(|| panic!("{}: expected {} in {:#?}", case.name, case.code, found));
        assert_eq!(hit.file, "env.toml", "{}", case.name);
        assert_eq!(hit.profile.as_deref(), case.profile, "{}", case.name);
        assert_eq!(hit.component.as_deref(), case.component, "{}", case.name);
        assert_eq!(hit.field.as_deref(), Some(case.field), "{}", case.name);
        assert!(
            !hit.fix.is_empty(),
            "{}: every diagnostic says how to fix it",
            case.name
        );
    }
}

#[test]
fn new_host_keep_awake_keys_require_upgrading_strict_old_readers_first() {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PreviousHostReader {
        #[serde(default)]
        pools: std::collections::BTreeMap<String, HostPoolSpec>,
    }

    let previous = toml::from_str::<PreviousHostReader>("").unwrap();
    assert!(previous.pools.is_empty());
    for setting in ["max_pin_seconds = 0\n", "max_exec_busy_seconds = 0\n"] {
        assert!(
            toml::from_str::<PreviousHostReader>(setting).is_err(),
            "the strict pre-change reader rejects {setting:?}"
        );
        assert!(
            toml::from_str::<HostEnvironmentsFile>(setting).is_ok(),
            "the current reader accepts {setting:?}"
        );
    }
    for seconds in [0, 1, i64::MAX as u64] {
        let text = format!("max_exec_busy_seconds = {seconds}\n");
        let host = toml::from_str::<HostEnvironmentsFile>(&text).unwrap();
        assert_eq!(host.max_exec_busy_seconds, Some(seconds));
    }
}

#[test]
fn a_host_pool_takes_its_limit_from_host_settings_only() {
    let toml = "schema = 1\n[pools.shared]\nscope = \"host\"\n[[profiles]]\nname = \"a\"\nunits = 2\ncomponents = [{ name = \"x\", runtime = \"process\", start = \"x\" }]\n";
    let file = parse(toml);
    let mut host = HostEnvironmentsFile::default();
    host.pools
        .insert("shared".into(), HostPoolSpec { limit: 1 });
    let found = validate(&file, &host, "env.toml", "host.toml");
    assert!(
        found.iter().any(|d| d.code == "units_exceed_limit"),
        "{found:#?}"
    );

    host.pools
        .insert("shared".into(), HostPoolSpec { limit: 8 });
    assert!(validate(&file, &host, "env.toml", "host.toml").is_empty());
    let compiled = compile(&file, &host, "a", "env.toml", &context()).unwrap();
    let pool = compiled.pool.expect("pool");
    assert_eq!(pool.lease_pool, "env:host:shared");
    assert_eq!(pool.limit, 8);
}

#[test]
fn unknown_keys_and_syntax_errors_carry_line_and_column() {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(
        PathBuf::from("t"),
        "env.toml",
        "schema = 1\n\n[[profiles]]\nname = \"a\"\nunitz = 1\n",
    );
    let diagnostic = loaded.value.expect_err("unknown key");
    assert_eq!(diagnostic.code, "parse_error");
    assert_eq!(diagnostic.line, Some(5), "{}", diagnostic.render());
    assert!(
        diagnostic.render().starts_with("env.toml:5:"),
        "{}",
        diagnostic.render()
    );
}

#[test]
fn components_compile_in_dependency_order_and_ties_keep_declaration_order() {
    let file = parse(NODE_COMPOSE);
    let isolated = file.profiles.iter().find(|p| p.name == "isolated").unwrap();
    let order: Vec<&str> = topological_order(&isolated.components)
        .unwrap()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(order, ["stack", "api"]);

    let file = parse(PYTHON_SQLITE);
    let compiled = compile(
        &file,
        &HostEnvironmentsFile::default(),
        "backend",
        "env.toml",
        &context(),
    )
    .unwrap();
    let names: Vec<&str> = compiled
        .components
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["db", "api"]);
    assert_eq!(compiled.components[0].runtime, Runtime::Hooks);
    assert_eq!(compiled.components[1].port_key.as_deref(), Some("port_api"));
}

#[test]
fn identity_is_stable_per_owner_and_profile() {
    let owner = EnvironmentOwner::Worktree {
        fingerprint: "a".into(),
    };
    let other = EnvironmentOwner::Worktree {
        fingerprint: "b".into(),
    };
    let id = environment_id("repo", &owner, "backend");
    assert_eq!(id, environment_id("repo", &owner, "backend"));
    assert_ne!(id, environment_id("repo", &other, "backend"));
    assert_ne!(id, environment_id("repo", &owner, "isolated"));
    assert_ne!(
        id,
        environment_id(
            "repo",
            &EnvironmentOwner::Ref { name: "a".into() },
            "backend"
        ),
        "a ref owner never collides with a worktree of the same spelling"
    );
    assert!(id.starts_with("env-") && id.len() == 16, "{id}");
}

#[test]
fn the_snapshot_digest_tracks_commands_not_where_the_file_came_from() {
    let file = parse(PYTHON_SQLITE);
    let host = HostEnvironmentsFile::default();
    let first = compile(&file, &host, "backend", "env.toml", &context()).unwrap();
    let mut moved = context();
    moved.source_commit = Some("b".repeat(40));
    moved.source_dirty = true;
    let second = compile(&file, &host, "backend", "env.toml", &moved).unwrap();
    assert_eq!(
        first.digest, second.digest,
        "a new commit alone is not a reconfiguration"
    );
    assert_eq!(
        second.source.commit.as_deref(),
        Some("b".repeat(40).as_str())
    );

    let edited = parse(&PYTHON_SQLITE.replace("db-drop.sh", "db-drop-v2.sh"));
    let third = compile(&edited, &host, "backend", "env.toml", &context()).unwrap();
    assert_ne!(first.digest, third.digest, "a changed destroy command is");
    assert_eq!(
        first.components[0].commands.destroy.as_deref(),
        Some("sh scripts/env/db-drop.sh"),
        "the snapshot carries the commands themselves"
    );
}

#[test]
fn a_profile_compiles_to_a_reservation_and_a_separate_capacity_request() {
    let file = parse(NODE_COMPOSE);
    let host = HostEnvironmentsFile::default();
    let isolated = compile(&file, &host, "isolated", "env.toml", &context()).unwrap();
    let reservation = isolated.reservation_request(None, 0);
    let kinds: Vec<(&str, &str)> = reservation
        .resources
        .iter()
        .map(|r| {
            (
                r.key.as_str(),
                match r.resource {
                    HostResourceKind::Namespace { .. } => "namespace",
                    HostResourceKind::TcpPort { .. } => "tcp_port",
                    HostResourceKind::Capacity { .. } => "capacity",
                    HostResourceKind::ExclusiveKey { .. } => "exclusive_key",
                },
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [(NAMESPACE_KEY, "namespace"), ("port_api", "tcp_port")]
    );
    assert!(
        !reservation
            .resources
            .iter()
            .any(|r| matches!(r.resource, HostResourceKind::Capacity { .. })),
        "parking must be able to release capacity without touching the reservation"
    );
    let capacity = isolated.capacity_request(None, 0).expect("capacity");
    assert_eq!(capacity.resources.len(), 1);
    assert_eq!(capacity.resources[0].key, CAPACITY_KEY);
    match &capacity.resources[0].resource {
        HostResourceKind::Capacity { pool, units, limit } => {
            assert_eq!(pool, "env:repo-0123456789abcdef:envs");
            assert_eq!((*units, *limit), (3, 6));
        }
        other => panic!("{other:?}"),
    }
    assert_ne!(reservation.request_id, capacity.request_id);

    let zero = compile(&file, &host, "static", "env.toml", &context()).unwrap();
    assert!(
        zero.capacity_request(None, 0).is_none(),
        "zero units never waits for capacity"
    );
}

/// `env plan` reports the coordinator's own plan for the compiled requests,
/// so it is identical to `resources plan` on the same request and host state.
#[test]
fn compiled_requests_are_accepted_and_planned_by_the_coordinator() {
    let directory = tempfile::tempdir().unwrap();
    let coordinator = HostResourceCoordinator::open(&directory.path().join("host.db")).unwrap();
    let file = parse(NODE_COMPOSE);
    let compiled = compile(
        &file,
        &HostEnvironmentsFile::default(),
        "isolated",
        "env.toml",
        &context(),
    )
    .unwrap();
    let reservation = compiled.reservation_request(None, 0);
    let first = coordinator.plan(&reservation).expect("valid reservation");
    let again = coordinator.plan(&reservation).unwrap();
    assert!(first.available, "{first:?}");
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&again).unwrap()
    );
    let namespace = first
        .proposed
        .iter()
        .find(|a| a.key == NAMESPACE_KEY)
        .unwrap();
    assert!(
        namespace.value.starts_with(&compiled.env_id),
        "{namespace:?}"
    );
    let capacity = coordinator
        .plan(&compiled.capacity_request(None, 0).unwrap())
        .expect("valid capacity");
    assert!(capacity.available);
    crate::resources::validate_host_resource_requirements(
        &reservation.resources,
        reservation.ttl_seconds,
    )
    .unwrap();
}

#[test]
fn variables_name_every_port_and_receipt_and_nothing_inherited() {
    let file = parse(PYTHON_SQLITE);
    let compiled = compile(
        &file,
        &HostEnvironmentsFile::default(),
        "backend",
        "env.toml",
        &context(),
    )
    .unwrap();
    let names = compiled.variable_names(&compiled.components[0]);
    for expected in [
        "AETHYME_ENV_ID",
        "AETHYME_ENV_NAMESPACE",
        "AETHYME_ENV_PORT_API",
        "AETHYME_ENV_URL_API",
        "AETHYME_ENV_RECEIPT_DB",
        "AETHYME_ENV_HOOK_ATTEMPT",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
    assert!(
        !names.iter().any(|n| n == "AETHYME_ENV_PORT_DB"),
        "db does not listen"
    );
}

/// #1292: a component's `entry_path` is appended to its URL variable, and
/// only to its own.
#[test]
fn an_entry_path_is_appended_to_the_component_url() {
    let file =
        parse(&PYTHON_SQLITE.replace("listen = true\n", "listen = true\nentry_path = \"/o/\"\n"));
    let compiled = compile(
        &file,
        &HostEnvironmentsFile::default(),
        "backend",
        "env.toml",
        &context(),
    )
    .unwrap();
    let bindings = super::exec::Bindings {
        ports: [("api".to_string(), 4000)].into(),
        ..Default::default()
    };
    let vars = super::exec::environment_variables(&compiled, &compiled.components[0], &bindings, 1);
    assert_eq!(vars["AETHYME_ENV_URL_API"], "http://127.0.0.1:4000/o/");
    assert_eq!(vars["AETHYME_ENV_PORT_API"], "4000");
}
