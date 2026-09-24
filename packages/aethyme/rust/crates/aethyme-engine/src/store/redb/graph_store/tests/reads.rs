use super::*;

struct ReadApiFixture {
    root: PathBuf,
    repository: RepositoryNode,
    directory: DirectoryNode,
    file: FileNode,
    test_file: FileNode,
    class: ClassNode,
    function: FunctionNode,
    doc: DocNode,
    config: ConfigNode,
    unresolved: UnresolvedNode,
    route: SurfaceNode,
    middleware: SurfaceNode,
    credential: SurfaceNode,
    proxy: SurfaceNode,
    behavior_test: SurfaceNode,
}

impl ReadApiFixture {
    fn read_only(&self) -> ReadOnlyGraphStore {
        GraphStore::open_read_only(&self.root).expect("read-only")
    }
}

impl Drop for ReadApiFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn read_api_fixture(name: &str) -> ReadApiFixture {
    let root = tmp_root(name);
    let store = GraphStore::open(&root).expect("open");

    let repository = RepositoryNode::new("Repo", root.to_str().unwrap());
    let area = AreaNode::new("Repo", "src", false);
    let directory = sample_directory("Repo", "src", Some("area:Repo:src"));
    let file = sample_file("Repo", "src/lib.rs", Some("area:Repo:src"));
    let test_file = sample_file("Repo", "tests/test_lib.rs", None);
    let class = sample_class(&file, "TokenLoader");
    let function = sample_function(&file, "LoadToken", Some(class.id.clone()));
    let unresolved = sample_unresolved(&file, "missing_call");
    let route = surface_node(SurfaceKind::RouteSurface, "src/lib.rs", "GET /api/token", 3);
    let middleware = surface_node(
        SurfaceKind::MiddlewareInstallation,
        "src/lib.rs",
        "TokenAuthMiddleware",
        4,
    );
    let credential = surface_node(
        SurfaceKind::CredentialOperation,
        "src/lib.rs",
        "issue token",
        18,
    );
    let proxy = surface_node(
        SurfaceKind::ProxySurface,
        "src/lib.rs",
        "https://auth.example.test",
        24,
    );
    let behavior_test = surface_node(
        SurfaceKind::BehaviorTestSurface,
        "tests/test_lib.rs",
        "test token auth",
        6,
    );
    let doc = DocNode::new(
        "Repo",
        "file:Repo:docs/auth.md",
        "docs/auth.md",
        "Auth",
        "markdown",
        Some(area.id.clone()),
    );
    let config = ConfigNode::new(
        "Repo",
        "file:Repo:pyproject.toml",
        "pyproject.toml",
        "toml",
        None,
    );

    let mut session = store.begin_index().expect("session");
    insert_repository(&mut session, &repository).expect("repository");
    insert_area(&mut session, &area).expect("area");
    insert_directory(&mut session, &directory).expect("directory");
    insert_file(&mut session, &file).expect("file");
    insert_file(&mut session, &test_file).expect("test file");
    insert_class(&mut session, &class).expect("class");
    insert_function(&mut session, &function).expect("function");
    insert_doc(&mut session, &doc).expect("doc");
    insert_config(&mut session, &config).expect("config");
    insert_unresolved(&mut session, &unresolved).expect("unresolved");
    for surface in [&route, &middleware, &credential, &proxy, &behavior_test] {
        insert_surface(&mut session, surface).expect("surface");
    }
    insert_edge(
        &mut session,
        &Edge::new(
            repository.id.as_str(),
            &area.id,
            EdgeKind::Contains,
            1000,
            "structure",
        ),
    )
    .expect("repo contains area");
    insert_edge(
        &mut session,
        &Edge::new(
            &area.id,
            &directory.id,
            EdgeKind::Contains,
            1000,
            "structure",
        ),
    )
    .expect("area contains directory");
    insert_edge(
        &mut session,
        &Edge::new(
            &file.id,
            function.id.as_str(),
            EdgeKind::Contains,
            1000,
            "structure",
        ),
    )
    .expect("file contains function");
    insert_edge(
        &mut session,
        &Edge::new(
            function.id.as_str(),
            &doc.id,
            EdgeKind::Documents,
            900,
            "docs",
        ),
    )
    .expect("function documents doc");
    insert_edge(
        &mut session,
        &Edge::new(
            function.id.as_str(),
            &config.id,
            EdgeKind::Configures,
            900,
            "config",
        ),
    )
    .expect("function configures config");
    insert_edge(
        &mut session,
        &Edge::new(
            &file.id,
            unresolved.id.as_str(),
            EdgeKind::Imports,
            850,
            "import",
        ),
    )
    .expect("file imports unresolved");
    insert_edge(
        &mut session,
        &Edge::new(
            &file.id,
            &area.id,
            EdgeKind::EntrypointFor,
            800,
            "entrypoint",
        ),
    )
    .expect("entrypoint");
    insert_edge(
        &mut session,
        &Edge::new(
            &file.id,
            route.id.as_str(),
            EdgeKind::Exposes,
            900,
            "surface-flow",
        ),
    )
    .expect("exposes route");
    insert_edge(
        &mut session,
        &Edge::new(
            &file.id,
            middleware.id.as_str(),
            EdgeKind::InstallsMiddleware,
            900,
            "surface-flow",
        ),
    )
    .expect("installs middleware");
    insert_edge(
        &mut session,
        &Edge::new(
            route.id.as_str(),
            credential.id.as_str(),
            EdgeKind::ValidatesCredential,
            900,
            "surface-flow",
        ),
    )
    .expect("route validates credential");
    insert_edge(
        &mut session,
        &Edge::new(
            middleware.id.as_str(),
            credential.id.as_str(),
            EdgeKind::Authorizes,
            900,
            "surface-flow",
        ),
    )
    .expect("middleware authorizes");
    insert_edge(
        &mut session,
        &Edge::new(
            credential.id.as_str(),
            route.id.as_str(),
            EdgeKind::IssuesCredential,
            900,
            "surface-flow",
        ),
    )
    .expect("issues credential");
    insert_edge(
        &mut session,
        &Edge::new(
            credential.id.as_str(),
            file.id.as_str(),
            EdgeKind::StoresCredential,
            900,
            "surface-flow",
        ),
    )
    .expect("stores credential");
    insert_edge(
        &mut session,
        &Edge::new(
            middleware.id.as_str(),
            credential.id.as_str(),
            EdgeKind::UsesCredential,
            900,
            "surface-flow",
        ),
    )
    .expect("uses credential");
    insert_edge(
        &mut session,
        &Edge::new(
            route.id.as_str(),
            proxy.id.as_str(),
            EdgeKind::ForwardsTo,
            900,
            "surface-flow",
        ),
    )
    .expect("forwards to proxy");
    insert_edge(
        &mut session,
        &Edge::new(
            proxy.id.as_str(),
            credential.id.as_str(),
            EdgeKind::RewritesHeader,
            900,
            "surface-flow",
        ),
    )
    .expect("rewrites header");
    insert_edge(
        &mut session,
        &Edge::new(
            &file.id,
            behavior_test.id.as_str(),
            EdgeKind::TestedBy,
            900,
            "surface-flow",
        ),
    )
    .expect("tested by");
    insert_risk(
        &mut session,
        &RiskFlag::new("src/", RiskArea::SharedCore, RiskLevel::Medium, "core path"),
    )
    .expect("risk");
    session.commit().expect("commit");

    store
        .set_repo_metadata(&RepoMetadata {
            root_path: root.to_string_lossy().to_string(),
            commit_hash: Some("abc123".to_string()),
            indexed_at_unix: 1,
            file_count: 2,
            languages: vec!["rust".to_string()],
        })
        .expect("metadata");
    drop(store);

    ReadApiFixture {
        root,
        repository,
        directory,
        file,
        test_file,
        class,
        function,
        doc,
        config,
        unresolved,
        route,
        middleware,
        credential,
        proxy,
        behavior_test,
    }
}

#[test]
fn read_api_get_node_resolves_typed_node_and_missing_id() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    match readonly
        .get_node(fixture.repository.id.as_str())
        .expect("repository node")
        .expect("present")
    {
        StoredNode::Repository(got) => assert_eq!(got, fixture.repository),
        other => panic!("expected repository node, got {other:?}"),
    }
    match readonly
        .get_node(fixture.directory.id.as_str())
        .expect("directory node")
        .expect("present")
    {
        StoredNode::Directory(got) => assert_eq!(got, fixture.directory),
        other => panic!("expected directory node, got {other:?}"),
    }
    match readonly
        .get_node(fixture.function.id.as_str())
        .expect("function node")
        .expect("present")
    {
        StoredNode::Function(got) => assert_eq!(got, fixture.function),
        other => panic!("expected function node, got {other:?}"),
    }
    match readonly
        .get_node(fixture.unresolved.id.as_str())
        .expect("unresolved node")
        .expect("present")
    {
        StoredNode::Unresolved(got) => assert_eq!(got, fixture.unresolved),
        other => panic!("expected unresolved node, got {other:?}"),
    }
    assert!(
        readonly
            .get_node("unknown:Repo:x")
            .expect("unknown")
            .is_none()
    );
}

#[test]
fn read_api_batch_display_and_area_projection() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();
    let ids = vec![
        fixture.repository.id.as_str(),
        "missing:Repo:x",
        fixture.function.id.as_str(),
    ];

    let nodes = readonly.get_nodes(&ids).expect("batch get");
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0].id(), fixture.repository.id);
    assert_eq!(nodes[1].id(), fixture.function.id.as_str());

    let display = readonly
        .node_display(fixture.function.id.as_str())
        .expect("display")
        .expect("function display");
    assert_eq!(display.kind, StoredNodeKind::Function);
    assert_eq!(display.display, "src/lib.rs::LoadToken");
    assert_eq!(display.area_id.as_deref(), Some("area:Repo:src"));
    assert_eq!(
        readonly
            .area_for_node(fixture.function.id.as_str())
            .expect("area"),
        Some("area:Repo:src".to_string())
    );
    assert_eq!(
        readonly.area_for_node("src/lib.rs").expect("path area"),
        Some("area:Repo:src".to_string())
    );
}

#[test]
fn read_api_find_symbols_is_case_insensitive_and_kind_filterable() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let symbols = readonly
        .find_symbols("LoadToken", None)
        .expect("find symbols");
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].id, fixture.function.id.to_string());
    assert_eq!(symbols[0].kind, StoredNodeKind::Function);
    assert!(
        readonly
            .find_symbols("LoadToken", Some(StoredNodeKind::Class))
            .expect("class filter")
            .is_empty()
    );
    assert!(
        readonly
            .find_symbols("missing_call", Some(StoredNodeKind::Unresolved))
            .expect("unresolved filter")
            .is_empty()
    );
}

#[test]
fn read_api_symbols_matching_returns_bounded_signal_candidates() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let exact = readonly.symbols_matching("LoadToken").expect("exact");
    let function_exact = exact
        .iter()
        .find(|candidate| candidate.symbol.id == fixture.function.id)
        .expect("function exact candidate");
    assert!(function_exact.signals.exact);
    assert!(function_exact.signals.case_insensitive);
    assert!(function_exact.rank > 0);

    let case_insensitive = readonly.symbols_matching("loadtoken").expect("case");
    let function_case = case_insensitive
        .iter()
        .find(|candidate| candidate.symbol.id == fixture.function.id)
        .expect("function case-insensitive candidate");
    assert!(!function_case.signals.exact);
    assert!(function_case.signals.case_insensitive);

    let prefix = readonly.symbols_matching("Load").expect("prefix");
    assert!(prefix.iter().any(|candidate| {
        candidate.symbol.id == fixture.function.id && candidate.signals.prefix
    }));

    let component = readonly.symbols_matching("token").expect("component");
    assert!(component.iter().any(|candidate| {
        candidate.symbol.id == fixture.function.id && candidate.signals.component
    }));
    assert!(component.iter().any(|candidate| {
        candidate.symbol.id == fixture.class.id && candidate.signals.component
    }));

    let path = readonly
        .symbols_matching_with(
            "src/",
            SymbolMatchOptions {
                limit: 10,
                ..SymbolMatchOptions::default()
            },
        )
        .expect("path");
    assert!(
        path.iter().any(|candidate| {
            candidate.symbol.id == fixture.function.id && candidate.signals.path
        })
    );

    let area = readonly.symbols_matching("src").expect("area");
    assert!(
        area.iter().any(|candidate| {
            candidate.symbol.id == fixture.function.id && candidate.signals.area
        })
    );

    let basename = readonly.symbols_matching("lib").expect("basename");
    assert!(basename.iter().any(|candidate| {
        candidate.symbol.id == fixture.function.id
            && candidate.signals.path
            && candidate.signals.basename
    }));

    let class_only = readonly
        .symbols_matching_with(
            "token",
            SymbolMatchOptions {
                limit: 10,
                kind: Some(StoredNodeKind::Class),
                ..SymbolMatchOptions::default()
            },
        )
        .expect("class kind filter");
    assert!(!class_only.is_empty());
    assert!(
        class_only
            .iter()
            .all(|candidate| candidate.symbol.kind == StoredNodeKind::Class)
    );
}

#[test]
fn read_api_symbols_matching_collects_stem_component_candidates() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let area = AreaNode::new("Repo", "includes", false);
    let file = sample_file(
        "Repo",
        "includes/Page/WikiPage.php",
        Some("area:Repo:includes"),
    );
    let function = sample_function(&file, "doViewUpdates", None);

    let mut session = store.begin_index().expect("session");
    insert_area(&mut session, &area).expect("area");
    insert_file(&mut session, &file).expect("file");
    insert_function(&mut session, &function).expect("function");
    session.commit().expect("commit");
    drop(store);

    let readonly = GraphStore::open_read_only(&root).expect("read-only");
    let hits = readonly
        .symbols_matching("viewing page")
        .expect("stem symbol match");
    let hit = hits
        .iter()
        .find(|candidate| candidate.symbol.id == function.id)
        .expect("doViewUpdates should be recalled via the view/viewing stem");
    assert!(hit.signals.component);
    assert!(hit.rank > 0);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn read_api_nodes_under_path_returns_typed_path_rows() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let nodes = readonly.nodes_under_path("src/").expect("nodes under src");
    let node_ids: BTreeSet<String> = nodes.iter().map(|node| node.id().to_string()).collect();
    assert!(node_ids.contains(&fixture.file.id));
    assert!(node_ids.contains(fixture.function.id.as_str()));
    assert!(node_ids.contains(fixture.class.id.as_str()));
    assert!(node_ids.contains(fixture.unresolved.id.as_str()));
    assert!(!node_ids.contains(&fixture.test_file.id));

    let src_nodes = readonly.nodes_under_path("src").expect("nodes under src");
    let src_ids: BTreeSet<String> = src_nodes.iter().map(|node| node.id().to_string()).collect();
    assert!(src_ids.contains(&fixture.directory.id));
}

#[test]
fn read_api_functions_under_path_returns_function_rows() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let functions = readonly
        .functions_under_path("src/")
        .expect("functions under src");
    assert_eq!(functions, vec![fixture.function.clone()]);
}

#[test]
fn read_api_resolve_file_path_is_exact() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let resolved = readonly
        .resolve_file_path("src/lib.rs")
        .expect("resolve file")
        .expect("present");
    assert_eq!(resolved, fixture.file);
    assert!(
        readonly
            .resolve_file_path("src")
            .expect("prefix is not exact")
            .is_none()
    );
    assert!(
        readonly
            .resolve_file_path("src/missing.rs")
            .expect("missing")
            .is_none()
    );
}

#[test]
fn read_api_neighbors_filters_direction_and_kind() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let incoming = readonly
        .neighbors(
            fixture.function.id.as_str(),
            NeighborDirection::Incoming,
            Some(EdgeKind::Contains),
        )
        .expect("incoming");
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].other.as_str(), fixture.file.id.as_str());

    let outgoing = readonly
        .neighbors(
            fixture.function.id.as_str(),
            NeighborDirection::Outgoing,
            Some(EdgeKind::Configures),
        )
        .expect("outgoing");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].other.as_str(), fixture.config.id.as_str());
    let unresolved_imports = readonly
        .neighbors(
            &fixture.file.id,
            NeighborDirection::Outgoing,
            Some(EdgeKind::Imports),
        )
        .expect("unresolved imports");
    assert_eq!(unresolved_imports.len(), 1);
    assert_eq!(
        unresolved_imports[0].other.as_str(),
        fixture.unresolved.id.as_str()
    );
    assert!(
        readonly
            .neighbors(
                fixture.function.id.as_str(),
                NeighborDirection::Outgoing,
                Some(EdgeKind::Imports),
            )
            .expect("wrong kind")
            .is_empty()
    );
}

#[test]
fn read_api_relation_docs_configs_and_risks() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let children = readonly
        .children(&fixture.file.id, Some(StoredNodeKind::Function))
        .expect("children");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, fixture.function.id.to_string());

    let parents = readonly
        .parents(fixture.function.id.as_str(), Some(StoredNodeKind::File))
        .expect("parents");
    assert_eq!(parents.len(), 1);
    assert_eq!(parents[0].id, fixture.file.id);

    let config_view = readonly
        .relation_view(fixture.function.id.as_str(), GraphRelation::Configs)
        .expect("config relation");
    assert_eq!(
        config_view.target.as_ref().map(|node| node.id.as_str()),
        Some(fixture.function.id.as_str())
    );
    assert_eq!(config_view.items.len(), 1);
    assert_eq!(config_view.items[0].node.id, fixture.config.id);

    let docs = readonly
        .docs_for(fixture.function.id.as_str())
        .expect("docs");
    assert_eq!(docs, vec![fixture.doc.clone()]);
    let configs = readonly
        .configs_for(fixture.function.id.as_str())
        .expect("configs");
    assert_eq!(configs, vec![fixture.config.clone()]);

    let risks = readonly
        .risk_for_node_or_path(fixture.function.id.as_str())
        .expect("risks");
    assert_eq!(risks.len(), 1);
    assert_eq!(risks[0].scope, "src/");
}

#[test]
fn read_api_task_anchor_and_usage_boundary_candidates_are_bounded() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let anchors = readonly
        .task_anchor_candidates(&["token", "src"], 5)
        .expect("anchors");
    let function_anchor = anchors
        .iter()
        .find(|candidate| candidate.node.id == fixture.function.id)
        .expect("function anchor");
    assert!(
        function_anchor
            .matched_tokens
            .contains(&"token".to_string())
    );
    assert!(function_anchor.matched_tokens.contains(&"src".to_string()));
    assert!(anchors.len() <= 5);

    let usage = readonly
        .usage_boundary_candidates("src/", Some(StoredNodeKind::Function), 5)
        .expect("usage");
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].node.id, fixture.function.id.to_string());
    assert_eq!(
        usage[0].symbol.as_ref().map(|symbol| symbol.name.as_str()),
        Some("LoadToken")
    );
}

#[test]
fn read_api_bounded_surface_flow_candidates_use_edge_kind_index() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let entrypoints = readonly
        .entrypoints_for_task(&["token"])
        .expect("entrypoints");
    let route = entrypoints
        .iter()
        .find(|candidate| candidate.node.id == fixture.route.id)
        .expect("route entrypoint");
    assert!(route.relation_kinds.contains(&EdgeKind::Exposes));
    assert!(entrypoints.len() <= FLOW_QUERY_LIMIT);

    let paths = readonly
        .surface_paths_for_behavior(&["token"])
        .expect("surface paths");
    let src_path = paths
        .iter()
        .find(|candidate| candidate.path == "src/lib.rs")
        .expect("src/lib.rs surface path");
    assert!(
        src_path
            .surfaces
            .iter()
            .any(|surface| surface.id == fixture.route.id)
    );
    assert!(
        src_path
            .relation_kinds
            .iter()
            .any(|kind| matches!(kind, EdgeKind::Exposes | EdgeKind::ValidatesCredential))
    );

    let credential = readonly
        .credential_flow_candidates(&["token"])
        .expect("credential flows");
    let credential_candidate = credential
        .iter()
        .find(|candidate| candidate.node.id == fixture.credential.id)
        .expect("credential operation");
    assert!(
        credential_candidate
            .relation_kinds
            .contains(&EdgeKind::IssuesCredential)
    );
    assert!(
        credential_candidate
            .relation_kinds
            .contains(&EdgeKind::StoresCredential)
    );

    let subsystems = readonly
        .subsystems_matching(&["token"])
        .expect("subsystems");
    assert!(
        subsystems
            .iter()
            .any(|candidate| candidate.path_prefix == "src")
    );
}

#[test]
fn read_api_flow_chains_tests_and_coverage_are_bounded() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let middleware = readonly
        .middleware_chain_for_route(fixture.route.id.as_str())
        .expect("middleware chain");
    assert!(
        middleware
            .roots
            .iter()
            .any(|root| root.id == fixture.route.id)
    );
    assert!(
        middleware
            .steps
            .iter()
            .any(|step| step.edge_kind == EdgeKind::InstallsMiddleware
                && step.to.id == fixture.middleware.id)
    );
    assert!(
        middleware
            .steps
            .iter()
            .any(|step| step.edge_kind == EdgeKind::ValidatesCredential
                && step.to.id == fixture.credential.id)
    );
    assert!(middleware.steps.len() <= FLOW_CHAIN_STEP_LIMIT);

    let forwarding = readonly
        .forwarding_chain_for_surface(fixture.route.id.as_str())
        .expect("forwarding chain");
    assert!(
        forwarding
            .steps
            .iter()
            .any(|step| step.edge_kind == EdgeKind::ForwardsTo && step.to.id == fixture.proxy.id)
    );
    assert!(
        forwarding
            .steps
            .iter()
            .any(|step| step.edge_kind == EdgeKind::RewritesHeader
                && step.to.id == fixture.credential.id)
    );

    let tests = readonly
        .tests_for_surface_or_symbol(fixture.function.id.as_str())
        .expect("tests for symbol");
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0].id, fixture.behavior_test.id.to_string());

    let coverage = readonly
        .coverage_for_task_class("token auth behavior")
        .expect("task-class coverage");
    assert!(!coverage.entrypoints.is_empty());
    assert!(!coverage.surface_paths.is_empty());
    assert!(!coverage.credential_flows.is_empty());
    assert_eq!(coverage.tests.len(), 1);
    assert!(!coverage.missing.contains(&"credential_flows".to_string()));
}

#[test]
fn read_api_overview_v2_returns_bounded_navigation_slice() {
    let fixture = read_api_fixture(function_name!());
    let readonly = fixture.read_only();

    let overview = readonly
        .overview_v2(OverviewV2Limits {
            directory_limit: 10,
            file_limit: 10,
            function_limit: 10,
            class_limit: 10,
            doc_limit: 10,
            config_limit: 10,
            unresolved_limit: 10,
            ..OverviewV2Limits::default()
        })
        .expect("overview v2");
    assert_eq!(overview.repo.as_ref().unwrap().file_count, 2);
    assert_eq!(overview.repository, Some(fixture.repository.clone()));
    assert_eq!(overview.directories, vec![fixture.directory.clone()]);
    assert_eq!(overview.entrypoint_paths, vec!["src/lib.rs".to_string()]);
    assert_eq!(overview.files.len(), 2);
    assert_eq!(overview.functions, vec![fixture.function.clone()]);
    assert_eq!(overview.classes, vec![fixture.class.clone()]);
    assert_eq!(overview.docs, vec![fixture.doc.clone()]);
    assert_eq!(overview.configs, vec![fixture.config.clone()]);
    assert_eq!(overview.unresolved, vec![fixture.unresolved.clone()]);
}

#[test]
fn overview_assembles_repo_areas_entrypoints_risks() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");

    let mut session = store.begin_index().expect("session");
    // 2 top-level areas + 1 nested.
    insert_area(&mut session, &AreaNode::new("R", "src", false)).expect("src");
    insert_area(&mut session, &AreaNode::new("R", "docs", false)).expect("docs");
    insert_area(&mut session, &AreaNode::new("R", "src/bin", false)).expect("nest");

    // Two files. main.rs is an entrypoint via an EntrypointFor edge.
    let main = sample_file("R", "src/main.rs", Some("area:R:src"));
    let lib = sample_file("R", "src/lib.rs", Some("area:R:src"));
    insert_file(&mut session, &main).expect("main");
    insert_file(&mut session, &lib).expect("lib");
    insert_edge(
        &mut session,
        &Edge::new(&main.id, "area:R:src", EdgeKind::EntrypointFor, 100, "ep"),
    )
    .expect("entrypoint");

    // Risks: one Low and one High. High should sort first.
    insert_risk(
        &mut session,
        &RiskFlag::new("low/", RiskArea::Auth, RiskLevel::Low, "minor"),
    )
    .expect("low");
    insert_risk(
        &mut session,
        &RiskFlag::new("high/", RiskArea::Secrets, RiskLevel::High, "secret"),
    )
    .expect("high");
    session.commit().expect("commit");

    store
        .set_repo_metadata(&RepoMetadata {
            root_path: "/tmp/r".into(),
            commit_hash: None,
            indexed_at_unix: 0,
            file_count: 2,
            languages: vec!["rust".into()],
        })
        .expect("meta");

    let ov = store.overview(20, 10, 20).expect("overview");
    assert_eq!(ov.repo.as_ref().unwrap().file_count, 2);

    // Only depth-1 areas in the overview.
    let area_prefixes: Vec<&str> = ov.areas.iter().map(|a| a.path_prefix.as_str()).collect();
    assert_eq!(area_prefixes, vec!["docs", "src"]);

    // Entrypoint resolves the file_id back to its path.
    assert_eq!(ov.entrypoint_paths, vec!["src/main.rs".to_string()]);

    // Risks sorted High-first.
    assert_eq!(ov.risks.len(), 2);
    assert_eq!(ov.risks[0].level, RiskLevel::High);
    assert_eq!(ov.risks[1].level, RiskLevel::Low);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn overview_respects_limits() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    let mut session = store.begin_index().expect("session");
    for i in 0..5 {
        insert_area(&mut session, &AreaNode::new("R", &format!("a{i}"), false)).expect("area");
        insert_risk(
            &mut session,
            &RiskFlag::new(format!("r{i}"), RiskArea::Auth, RiskLevel::Low, "x"),
        )
        .expect("risk");
    }
    session.commit().expect("commit");

    let ov = store.overview(2, 10, 3).expect("overview");
    assert_eq!(ov.areas.len(), 2);
    assert_eq!(ov.risks.len(), 3);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn schema_mismatch_is_reported() {
    let root = tmp_root(function_name!());
    // Open once at the real version.
    let _ = GraphStore::open(&root).expect("open");

    // Stomp the sentinel to a different version, then reopen.
    {
        let db_path = root.join(".aethyme").join(DB_FILE_NAME);
        let db = Database::create(&db_path).expect("reopen raw");
        let txn = db.begin_write().expect("write txn");
        {
            let mut meta = txn.open_table(META).expect("META");
            meta.insert(META_KEY_SCHEMA_VERSION, &999u32.to_le_bytes()[..])
                .expect("stomp");
        }
        txn.commit().expect("commit");
    }

    match GraphStore::open(&root) {
        Ok(_) => panic!("expected SchemaMismatch, got Ok"),
        Err(GraphStoreError::SchemaMismatch { found, expected }) => {
            assert_eq!(found, 999);
            assert_eq!(expected, SCHEMA_VERSION);
        }
        Err(other) => panic!("expected SchemaMismatch, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn incompatible_redb_file_format_is_reported() {
    let root = tmp_root(function_name!());
    let store = GraphStore::open(&root).expect("open");
    drop(store);
    mark_graph_store_as_redb_v2(&root);

    match GraphStore::open(&root) {
        Ok(_) => panic!("expected IncompatibleRedbFileFormat, got Ok"),
        Err(GraphStoreError::IncompatibleRedbFileFormat { path, found }) => {
            assert_eq!(found, 2);
            assert_eq!(path, root.join(".aethyme").join(DB_FILE_NAME));
        }
        Err(other) => panic!("expected IncompatibleRedbFileFormat, got {other:?}"),
    }

    let message = match GraphStore::open(&root) {
        Ok(_) => panic!("expected old redb format to fail"),
        Err(err) => err.to_string(),
    };
    assert!(message.contains("aethyme-engine-cli index --repo <repo>"));
    assert!(message.contains(".aethyme/graph/"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reset_replaces_incompatible_graph_store_without_touching_fragments() {
    let root = tmp_root(function_name!());
    let fragment_marker = root.join(".aethyme/graph/fragments.marker");
    std::fs::create_dir_all(fragment_marker.parent().unwrap()).expect("fragment dir");
    std::fs::write(&fragment_marker, b"source-of-truth").expect("fragment marker");

    let store = GraphStore::open(&root).expect("open");
    drop(store);
    mark_graph_store_as_redb_v2(&root);

    let incompatible =
        GraphStore::detect_incompatible_file_format(&root).expect("old redb format detected");
    assert_eq!(incompatible.found_redb_format, 2);

    let rebuilt = GraphStore::reset(&root).expect("reset");
    assert!(rebuilt.path().exists(), "graph_store.redb recreated");
    assert_eq!(
        std::fs::read(&fragment_marker).expect("fragment marker"),
        b"source-of-truth"
    );

    let _ = std::fs::remove_dir_all(&root);
}
