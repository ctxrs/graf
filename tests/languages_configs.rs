use graf::{
    languages::configs::{parse, supports},
    model::{FileFacts, Node},
};

fn facts(path: &str, text: &str) -> FileFacts {
    let f = parse(path, text, "fixture").unwrap().unwrap();
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    let ids: std::collections::HashSet<_> = f.nodes.iter().map(|n| &n.id).collect();
    assert!(
        f.edges
            .iter()
            .all(|e| ids.contains(&e.source) && ids.contains(&e.target))
    );
    assert!(f.references.iter().all(|r| ids.contains(&r.source)));
    f
}

mod cargo_package_context {
    use graf::{
        languages::configs::{CargoPackageContext, parse},
        model::{Coverage, Edge, GraphSnapshot, Node},
        store::Store,
    };
    use std::{collections::BTreeMap, fs, path::PathBuf};

    struct Fixture(tempfile::TempDir);
    impl Fixture {
        fn new() -> Self {
            let f = Self(tempfile::tempdir().unwrap());
            fs::create_dir(f.root()).unwrap();
            f
        }
        fn root(&self) -> PathBuf {
            self.0.path().join("repo")
        }
        fn write(&self, path: &str, source: &str) {
            let path = self.root().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
        fn package(&self, path: &str, name: &str, extra: &str) {
            self.write(
                path,
                &format!("[package]\nname = '{name}'\nversion = '0.1.0'\n{extra}"),
            );
        }
        fn context(&self, paths: &[&str]) -> CargoPackageContext {
            CargoPackageContext::discover(
                &self.root(),
                &paths.iter().map(|p| (*p).into()).collect::<Vec<_>>(),
            )
            .unwrap()
        }
        // The production Store exercises rebinding and owner replacement; the caller
        // supplies the exact inventory and folds the context into manifest stamps.
        fn update(&self, paths: &[&str]) -> GraphSnapshot {
            let context = self.context(paths);
            let mut store = Store::create(&self.0.path().join("graph.db")).unwrap();
            let mut old: BTreeMap<_, _> = store
                .file_stamps()
                .unwrap()
                .into_iter()
                .map(|s| (s.path, s.hash))
                .collect();
            let mut changed = vec![];
            for path in paths
                .iter()
                .filter(|p| p.rsplit('/').next() == Some("Cargo.toml"))
            {
                let source = fs::read_to_string(self.root().join(path)).unwrap();
                let hash = format!(
                    "{}:{}",
                    blake3::hash(source.as_bytes()),
                    context.fingerprint()
                );
                if old.remove(*path).as_ref() == Some(&hash) {
                    continue;
                }
                let mut facts = parse(path, &source, &hash).unwrap().unwrap();
                context.apply(&mut facts);
                let once = serde_json::to_value(&facts).unwrap();
                context.apply(&mut facts);
                assert_eq!(once, serde_json::to_value(&facts).unwrap());
                changed.push(facts);
            }
            store
                .apply_native(
                    self.root().to_str().unwrap(),
                    changed,
                    old.into_keys().collect(),
                    Coverage::default(),
                )
                .unwrap();
            let graph = store.snapshot().unwrap();
            assert!(
                graph
                    .edges
                    .iter()
                    .all(|e| graph.nodes.iter().any(|n| n.id == e.source)
                        && graph.nodes.iter().any(|n| n.id == e.target))
            );
            graph
        }
    }
    fn package<'a>(graph: &'a GraphSnapshot, path: &str) -> &'a Node {
        graph
            .nodes
            .iter()
            .find(|n| n.file == path && n.kind == "package")
            .unwrap()
    }
    fn dependencies(graph: &GraphSnapshot) -> Vec<&Edge> {
        graph
            .edges
            .iter()
            .filter(|e| e.relation == "crate_depends_on")
            .collect()
    }
    fn linked(graph: &GraphSnapshot, source: &str, target: &str) -> bool {
        let source = &package(graph, source).id;
        let target = &package(graph, target).id;
        dependencies(graph)
            .iter()
            .any(|e| &e.source == source && &e.target == target)
    }
    fn members<'a>(graph: &'a GraphSnapshot, manifest: &str) -> Vec<&'a str> {
        let root = graph
            .nodes
            .iter()
            .find(|n| n.file == manifest && n.kind == "module")
            .unwrap();
        let mut paths: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.source == root.id && e.relation == "contains")
            .filter_map(|e| {
                graph
                    .nodes
                    .iter()
                    .find(|n| n.id == e.target && n.kind == "package")
            })
            .map(|n| n.file.as_str())
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn virtual_workspace_links_manifest_only_packages_and_inherited_renames() {
        let f = Fixture::new();
        f.write("Cargo.toml", "[workspace]\nmembers = ['crates/*']\n[workspace.dependencies]\nshared = { package = 'storage-core', path = 'crates/storage' }\nexternal = '1'\n");
        f.package("crates/app/Cargo.toml", "app", "[dependencies]\ndb = { package = 'storage-core', path = '../storage' }\nshared = { workspace = true }\nexternal = { workspace = true }\n");
        f.package(
            "crates/storage/Cargo.toml",
            "storage-core",
            "[lib]\nname = 'source_alias'\n",
        );
        let paths = [
            "Cargo.toml",
            "crates/app/Cargo.toml",
            "crates/storage/Cargo.toml",
        ];
        let graph = f.update(&paths);
        assert_eq!(
            graph.nodes.iter().filter(|n| n.kind == "package").count(),
            2
        );
        assert_eq!(members(&graph, "Cargo.toml"), paths[1..]);
        assert!(linked(&graph, paths[1], paths[2]));
        let target = package(&graph, paths[2]);
        assert_eq!(
            target.binding_key.as_deref(),
            Some("cargo:manifest:crates/storage/Cargo.toml:storage-core")
        );
        assert_eq!(target.metadata["workspace_manifest"], "Cargo.toml");
        let parsed = parse(
            paths[2],
            &fs::read_to_string(f.root().join(paths[2])).unwrap(),
            "original",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            target.id,
            parsed
                .nodes
                .iter()
                .find(|n| n.kind == "package")
                .unwrap()
                .id
        );
        let edges = dependencies(&graph);
        assert_eq!(edges.len(), 2);
        let mut aliases: Vec<_> = edges
            .iter()
            .map(|e| e.metadata["alias"].as_str().unwrap())
            .collect();
        aliases.sort();
        assert_eq!(aliases, ["db", "shared"]);
        for edge in edges {
            assert_eq!(edge.metadata["context"], "cargo_dependency");
            assert_eq!(edge.file.as_deref(), Some(paths[1]));
            assert_eq!(edge.line, Some(1));
            assert!(graph.edges.iter().any(|e| e.source == edge.source
                && e.target == edge.target
                && e.relation == "depends_on"
                && e.id != edge.id));
        }
        assert!(
            graph
                .nodes
                .iter()
                .all(|n| n.label != "external" && n.label != "source_alias")
        );
        let second = f.update(&paths);
        assert_eq!(dependencies(&second).len(), 2);
    }

    #[test]
    fn root_packages_explicit_workspace_paths_and_nested_workspaces_stay_separate() {
        let f = Fixture::new();
        f.package(
            "Cargo.toml",
            "root",
            "[workspace]\nmembers = ['crates/*', 'nested/**']\n",
        );
        f.package("crates/member/Cargo.toml", "member", "[dependencies]\nroot = { path = '../..' }\nforeign = { package = 'worker', path = '../../nested/worker' }\n");
        f.write("nested/Cargo.toml", "[workspace]\nmembers = ['worker']\n");
        f.package(
            "nested/worker/Cargo.toml",
            "worker",
            "[dependencies]\nroot = { path = '../..' }\n",
        );
        f.write("separate/Cargo.toml", "[workspace]\nmembers = ['../loose/*']\n[workspace.dependencies]\npeer = { path = '../loose/peer' }\n");
        f.package(
            "loose/member/Cargo.toml",
            "member",
            "workspace = '../../separate'\n[dependencies]\npeer = { workspace = true }\n",
        );
        f.package(
            "loose/peer/Cargo.toml",
            "peer",
            "workspace = '../../separate'\n",
        );
        let paths = [
            "Cargo.toml",
            "crates/member/Cargo.toml",
            "nested/Cargo.toml",
            "nested/worker/Cargo.toml",
            "separate/Cargo.toml",
            "loose/member/Cargo.toml",
            "loose/peer/Cargo.toml",
        ];
        let graph = f.update(&paths);
        assert_eq!(dependencies(&graph).len(), 2);
        assert!(linked(&graph, paths[1], paths[0]));
        assert!(linked(&graph, paths[5], paths[6]));
        assert_eq!(
            package(&graph, paths[0]).binding_key.as_deref(),
            Some("cargo:manifest:Cargo.toml:root")
        );
        assert_eq!(members(&graph, paths[0]), [paths[0], paths[1]]);
        assert_eq!(members(&graph, paths[2]), [paths[3]]);
        assert_eq!(members(&graph, paths[4]), [paths[5], paths[6]]);
        assert_eq!(
            package(&graph, paths[3]).metadata["workspace_manifest"],
            paths[2]
        );
        let parsed = parse(
            paths[1],
            &fs::read_to_string(f.root().join(paths[1])).unwrap(),
            "h",
        )
        .unwrap()
        .unwrap();
        assert!(
            parsed
                .references
                .iter()
                .any(|r| r.candidate_keys == ["cargo:manifest:Cargo.toml:root"])
        );
    }

    #[test]
    fn declared_internal_edges_require_visible_members_static_paths_and_valid_names() {
        let f = Fixture::new();
        f.write("Cargo.toml", "[workspace]\nmembers = ['crates/*', 'broken/*']\nexclude = ['crates/excluded*']\n[workspace.dependencies]\ninherited = { package = 'storage', path = 'crates/storage', optional = true }\noverridden = { package = 'storage', path = 'crates/storage' }\n");
        f.package(
            "crates/app/Cargo.toml",
            "app",
            r#"
[dependencies]
valid = { package = 'storage', path = '../storage', optional = false }
registry = { package = 'storage', version = '1' }
optional_registry = { package = 'storage', version = '1', optional = true }
git = { package = 'storage', git = 'https://example.invalid/repo', path = '../storage' }
named_registry = { package = 'storage', registry = 'alternate', path = '../storage' }
optional = { package = 'storage', path = '../storage', optional = true }
invalid_optional = { package = 'storage', path = '../storage', optional = 'false' }
inherited = { workspace = true, optional = false }
overridden = { workspace = true, path = '../storage' }
false_workspace = { workspace = false, package = 'storage', path = '../storage' }
missing_inheritance = { workspace = true }
wrong_name = { package = 'different', path = '../storage' }
empty_name = { package = '', path = '../storage' }
excluded = { path = '../excluded-one' }
ignored = { path = '../ignored' }
deep = { path = '../nested/deep' }
isolated = { path = '../isolated' }
duplicate = { package = 'duplicate', path = '../duplicate-one' }
optional_duplicate = { package = 'duplicate', path = '../duplicate-one', optional = true }
broken_child = { path = '../../broken/child' }
dynamic = { package = 'storage', path = '${ROOT}/storage' }
outside = { path = '../../../outside' }
absolute = { package = 'storage', path = '/storage' }
self_dep = { package = 'app', path = '.' }
[target.'cfg(unix)'.dependencies]
conditional = { package = 'storage', path = '../storage' }
optional_conditional = { package = 'storage', path = '../storage', optional = true }
[dev-dependencies]
dev = { package = 'storage', path = '../storage' }
optional_dev = { package = 'storage', path = '../storage', optional = true }
[build-dependencies]
build = { package = 'storage', path = '../storage' }
optional_build = { package = 'storage', path = '../storage', optional = true }
"#,
        );
        f.package("crates/storage/Cargo.toml", "storage", "");
        f.package("crates/excluded-one/Cargo.toml", "excluded", "");
        f.package("crates/ignored/Cargo.toml", "ignored", "");
        f.package("crates/nested/deep/Cargo.toml", "deep", "");
        f.package("crates/isolated/Cargo.toml", "isolated", "[workspace]\n");
        f.package("crates/duplicate-one/Cargo.toml", "duplicate", "");
        f.package("crates/duplicate-two/Cargo.toml", "duplicate", "");
        f.write("broken/Cargo.toml", "[workspace\n");
        f.package("broken/child/Cargo.toml", "broken_child", "");
        fs::create_dir(f.0.path().join("outside")).unwrap();
        fs::write(
            f.0.path().join("outside/Cargo.toml"),
            "[package]\nname = 'outside'\n",
        )
        .unwrap();
        let paths = [
            "Cargo.toml",
            "crates/app/Cargo.toml",
            "crates/storage/Cargo.toml",
            "crates/excluded-one/Cargo.toml",
            "crates/nested/deep/Cargo.toml",
            "crates/isolated/Cargo.toml",
            "crates/duplicate-one/Cargo.toml",
            "crates/duplicate-two/Cargo.toml",
            "broken/Cargo.toml",
            "broken/child/Cargo.toml",
        ];
        let graph = f.update(&paths);
        let edges = dependencies(&graph);
        let aliases: std::collections::BTreeSet<_> = edges
            .iter()
            .map(|e| e.metadata["alias"].as_str().unwrap())
            .collect();
        assert_eq!(
            aliases,
            std::collections::BTreeSet::from(["inherited", "optional", "valid"])
        );
        for edge in &edges {
            if edge.metadata["alias"] == "valid" {
                assert_eq!(
                    edge.metadata,
                    serde_json::json!({"context":"cargo_dependency", "alias":"valid"})
                );
            } else {
                assert_eq!(edge.metadata["optional"], true);
                assert_eq!(edge.metadata["activation"], "not_evaluated");
            }
        }
        assert!(linked(&graph, paths[1], paths[2]));
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.relation == "depends_on")
                .count(),
            3
        );
        for path in [paths[3], paths[4], paths[9]] {
            assert!(package(&graph, path).metadata["workspace_manifest"].is_null());
            assert!(!members(&graph, paths[0]).contains(&path));
        }
        assert!(
            graph
                .nodes
                .iter()
                .all(|n| n.label != "ignored" && n.label != "outside")
        );
        // Removing the optional declaration changes evidence, not topology.
        let root = fs::read_to_string(f.root().join("Cargo.toml"))
            .unwrap()
            .replace("optional = true", "optional = false");
        f.write("Cargo.toml", &root);
        let graph = f.update(&paths);
        assert_eq!(dependencies(&graph).len(), 3);
        let inherited = dependencies(&graph)
            .into_iter()
            .find(|e| e.metadata["alias"] == "inherited")
            .unwrap();
        assert_eq!(
            inherited.metadata,
            serde_json::json!({"context":"cargo_dependency", "alias":"inherited"})
        );
    }

    #[test]
    fn optional_declarations_refresh_evidence_and_remove_deleted_members() {
        let f = Fixture::new();
        let workspace = "[workspace]\nmembers = ['app', 'storage']\n[workspace.dependencies]\nshared = { package = 'storage-core', path = 'storage', optional = true }\nlocal_flag = { package = 'storage-core', path = 'storage' }\n";
        let declarations = "[dependencies]\ndirect = { package = 'storage-core', path = '../storage', optional = true }\nshared = { workspace = true }\nlocal_flag = { workspace = true, optional = true }\n";
        f.write("Cargo.toml", workspace);
        f.package("app/Cargo.toml", "app", declarations);
        f.package("storage/Cargo.toml", "storage-core", "");
        let paths = ["Cargo.toml", "app/Cargo.toml", "storage/Cargo.toml"];
        let first = f.update(&paths);
        let target = package(&first, paths[2]).id.clone();
        let source = package(&first, paths[1]).id.clone();
        let edges = dependencies(&first);
        assert_eq!(edges.len(), 3);
        let identities: std::collections::BTreeSet<_> =
            edges.iter().map(|e| e.id.clone()).collect();
        for edge in edges {
            assert_eq!(edge.source, source);
            assert_eq!(edge.target, target);
            assert_eq!(edge.file.as_deref(), Some(paths[1]));
            assert_eq!(edge.metadata["optional"], true);
            assert_eq!(edge.metadata["activation"], "not_evaluated");
            assert!(first.edges.iter().any(|r| r.relation == "depends_on"
                && r.source == source
                && r.target == target
                && r.id != edge.id));
        }
        // The package context never adds aliases/targets to real source import facts.
        let mut rust =
            graf::languages::parse("app/src/lib.rs", "use direct::Thing; pub fn run() {}", "h")
                .unwrap()
                .unwrap();
        let before = serde_json::to_value(&rust).unwrap();
        f.context(&paths).apply(&mut rust);
        assert_eq!(before, serde_json::to_value(&rust).unwrap());

        let optional_fingerprint = f.context(&paths).fingerprint().to_owned();
        f.write(
            paths[0],
            &workspace.replace("optional = true", "optional = false"),
        );
        f.package(
            paths[1],
            "app",
            &declarations.replace("optional = true", "optional = false"),
        );
        let required = f.update(&paths);
        assert_ne!(f.context(&paths).fingerprint(), optional_fingerprint);
        assert_eq!(
            dependencies(&required)
                .into_iter()
                .map(|e| e.id.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            identities
        );
        for edge in dependencies(&required) {
            assert_eq!(
                edge.metadata,
                serde_json::json!({"context":"cargo_dependency", "alias":edge.metadata["alias"]})
            );
        }
        // Flip back: neither the old evidence nor membership may survive replacement.
        f.write(paths[0], workspace);
        f.package(paths[1], "app", declarations);
        let optional_again = f.update(&paths);
        assert_eq!(dependencies(&optional_again).len(), 3);
        assert!(
            dependencies(&optional_again)
                .iter()
                .all(|e| e.metadata["optional"] == true
                    && e.metadata["activation"] == "not_evaluated")
        );
        f.write(
            paths[0],
            &workspace.replace(
                "members = ['app', 'storage']",
                "members = ['app', 'storage']\nexclude = ['storage']",
            ),
        );
        let excluded = f.update(&paths);
        assert_eq!(package(&excluded, paths[2]).id, target);
        assert!(excluded.edges.iter().all(|e| e.source != source
            || !matches!(e.relation.as_str(), "depends_on" | "crate_depends_on")));
        f.write(paths[0], workspace);
        assert_eq!(dependencies(&f.update(&paths)).len(), 3);
        fs::remove_file(f.root().join(paths[2])).unwrap();
        let deleted = f.update(&paths[..2]);
        assert!(deleted.nodes.iter().all(|n| n.id != target));
        assert!(deleted.edges.iter().all(|e| e.source != source
            || !matches!(e.relation.as_str(), "depends_on" | "crate_depends_on")));
        f.package(paths[2], "storage-core", "");
        let restored = f.update(&paths);
        assert_eq!(dependencies(&restored).len(), 3);
        assert!(
            dependencies(&restored)
                .iter()
                .all(|e| e.target == target && e.metadata["activation"] == "not_evaluated")
        );
    }

    #[test]
    fn manifest_updates_deletion_and_exclusion_refresh_dependency_owners() {
        let f = Fixture::new();
        f.write("Cargo.toml", "[workspace]\nmembers = ['*']\n[workspace.dependencies]\nstore = { package = 'storage', path = 'one' }\n");
        f.package(
            "app/Cargo.toml",
            "app",
            "[dependencies]\nstore = { workspace = true }\n",
        );
        f.package("one/Cargo.toml", "storage", "");
        f.package("two/Cargo.toml", "replacement", "");
        let paths = [
            "Cargo.toml",
            "app/Cargo.toml",
            "one/Cargo.toml",
            "two/Cargo.toml",
        ];
        let original = f.update(&paths);
        assert!(linked(&original, paths[1], paths[2]));
        let original_id = package(&original, paths[2]).id.clone();
        let original_hash = f.context(&paths).fingerprint().to_owned();
        let root = fs::read_to_string(f.root().join(paths[0])).unwrap();
        f.write(
            paths[0],
            &root.replace("members = ['*']", "members = ['*']\nexclude = ['one']"),
        );
        let excluded = f.update(&paths);
        assert!(dependencies(&excluded).is_empty());
        assert_eq!(package(&excluded, paths[2]).id, original_id);
        assert!(!members(&excluded, paths[0]).contains(&paths[2]));
        assert_ne!(f.context(&paths).fingerprint(), original_hash);
        f.write(paths[0], &root);
        assert!(linked(&f.update(&paths), paths[1], paths[2]));
        // An ignored manifest stays on disk but disappears from exact membership.
        let without_target = [paths[0], paths[1], paths[3]];
        let ignored = f.update(&without_target);
        assert!(dependencies(&ignored).is_empty());
        assert!(ignored.nodes.iter().all(|n| n.id != original_id));
        assert!(linked(&f.update(&paths), paths[1], paths[2]));
        fs::remove_file(f.root().join(paths[2])).unwrap();
        assert!(dependencies(&f.update(&without_target)).is_empty());
        f.package(paths[2], "storage", "");
        assert!(linked(&f.update(&paths), paths[1], paths[2]));
        f.write(
            paths[0],
            &root.replace(
                "package = 'storage', path = 'one'",
                "package = 'replacement', path = 'two'",
            ),
        );
        let redirected = f.update(&paths);
        assert_eq!(dependencies(&redirected).len(), 1);
        assert!(linked(&redirected, paths[1], paths[3]));
        assert!(!linked(&redirected, paths[1], paths[2]));
        f.package(paths[3], "renamed", "");
        assert!(dependencies(&f.update(&paths)).is_empty());
        f.write(
            paths[0],
            &root.replace(
                "package = 'storage', path = 'one'",
                "package = 'renamed', path = 'two'",
            ),
        );
        assert!(linked(&f.update(&paths), paths[1], paths[3]));
    }

    #[test]
    fn context_fingerprint_uses_only_manifest_inventory_and_keeps_source_facts_intact() {
        let a = Fixture::new();
        let b = Fixture::new();
        for f in [&a, &b] {
            f.write("Cargo.toml", "[workspace]\nmembers = ['part']\n");
            f.package("part/Cargo.toml", "part", "");
        }
        let paths = ["Cargo.toml", "part/Cargo.toml"];
        let original = a.context(&paths).fingerprint().to_owned();
        assert_eq!(
            original,
            b.context(&[paths[1], paths[0], paths[0]]).fingerprint()
        );
        a.write("part/src/lib.rs", "pub fn example() {}\n");
        a.package("ignored/Cargo.toml", "ignored", "");
        a.write("Cargo.lock", "version = 4\n");
        assert_eq!(
            original,
            a.context(&[paths[0], paths[1], "part/src/lib.rs", "Cargo.lock"])
                .fingerprint()
        );
        a.write("part/src/lib.rs", "pub fn changed() {}\n");
        assert_eq!(original, a.context(&paths).fingerprint());
        assert_ne!(original, a.context(&[paths[0]]).fingerprint());
        assert_ne!(
            original,
            a.context(&[paths[0], paths[1], "missing/Cargo.toml"])
                .fingerprint()
        );
        let mut source_facts = graf::languages::parse(
            "part/src/lib.rs",
            "use other::Thing; pub fn example() {}",
            "h",
        )
        .unwrap()
        .unwrap();
        let before = serde_json::to_value(&source_facts).unwrap();
        a.context(&paths).apply(&mut source_facts);
        assert_eq!(before, serde_json::to_value(&source_facts).unwrap());
        for path in [
            "../Cargo.toml",
            "/Cargo.toml",
            "part/../Cargo.toml",
            "part//Cargo.toml",
        ] {
            assert!(CargoPackageContext::discover(&a.root(), &[path.into()]).is_err());
        }
    }

    #[test]
    fn manifest_only_dependency_chains_are_not_truncated() {
        let f = Fixture::new();
        f.write("Cargo.toml", "[workspace]\nmembers = ['crates/*']\n");
        let mut paths = vec!["Cargo.toml".to_owned()];
        for index in 0..200 {
            let path = format!("crates/p{index:03}/Cargo.toml");
            let extra = if index == 199 {
                String::new()
            } else {
                format!(
                    "[dependencies]\np{next:03} = {{ path = '../p{next:03}' }}\n",
                    next = index + 1
                )
            };
            f.package(&path, &format!("p{index:03}"), &extra);
            paths.push(path);
        }
        let graph = f.update(&paths.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(
            graph.nodes.iter().filter(|n| n.kind == "package").count(),
            200
        );
        assert_eq!(members(&graph, "Cargo.toml").len(), 200);
        assert_eq!(dependencies(&graph).len(), 199);
        for index in 0..199 {
            assert!(linked(&graph, &paths[index + 1], &paths[index + 2]));
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_manifests_never_supply_internal_dependency_targets() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        f.package("Cargo.toml", "root", "[workspace]\nmembers = ['*']\n[dependencies]\nvalid = { path = 'valid' }\ndirectory = { package = 'external', path = 'linked' }\nfile = { package = 'external', path = 'file' }\nalias = { package = 'valid', path = 'alias' }\n");
        f.package("valid/Cargo.toml", "valid", "");
        let outside = f.0.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("Cargo.toml"), "[package]\nname = 'external'\n").unwrap();
        symlink(&outside, f.root().join("linked")).unwrap();
        fs::create_dir(f.root().join("file")).unwrap();
        symlink(outside.join("Cargo.toml"), f.root().join("file/Cargo.toml")).unwrap();
        symlink(f.root().join("valid"), f.root().join("alias")).unwrap();
        let paths = [
            "Cargo.toml",
            "valid/Cargo.toml",
            "linked/Cargo.toml",
            "file/Cargo.toml",
            "alias/Cargo.toml",
        ];
        let context = f.context(&paths);
        let mut facts = parse(
            paths[0],
            &fs::read_to_string(f.root().join(paths[0])).unwrap(),
            "h",
        )
        .unwrap()
        .unwrap();
        context.apply(&mut facts);
        assert_eq!(
            facts
                .edges
                .iter()
                .filter(|e| e.relation == "crate_depends_on")
                .count(),
            1
        );
        assert_eq!(
            facts
                .references
                .iter()
                .filter(|r| r.relation == "depends_on" && !r.candidate_keys.is_empty())
                .count(),
            1
        );
        assert!(
            facts
                .edges
                .iter()
                .filter(|e| e.relation == "crate_depends_on")
                .all(|e| e.metadata["alias"] == "valid")
        );
    }
}
fn node<'a>(f: &'a FileFacts, label: &str) -> &'a Node {
    f.nodes
        .iter()
        .find(|n| n.label == label)
        .unwrap_or_else(|| panic!("missing {label}: {:?}", f.nodes))
}
fn labels(f: &FileFacts) -> Vec<&str> {
    f.nodes.iter().map(|n| n.label.as_str()).collect()
}

#[test]
fn exact_dispatch_and_malformed_input_boundaries() {
    for name in [
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "apm.yaml",
        "package.json",
        "deno.jsonc",
        ".babelrc",
        "tsconfig.build.json",
        "schema.sql",
        "main.tf",
        "prod.tfvars",
        "build.hcl",
        "app.sln",
        "app.slnx",
        "app.csproj",
        "app.fsproj",
        "app.vbproj",
        ".mcp.json",
    ] {
        assert!(supports(name), "{name}");
    }
    for name in ["ordinary.json", "notes.yaml", "manual.toml", "Cargo.lock"] {
        assert!(!supports(name));
        assert!(parse(name, "{}", "h").unwrap().is_none());
    }
    assert!(parse("../Cargo.toml", "", "h").is_err());
    for (path, src) in [
        ("Cargo.toml", "[package"),
        ("package.json", "{broken"),
        (
            "app.csproj",
            "<!DOCTYPE p [<!ENTITY e SYSTEM 'file:///missing'>]><Project/>",
        ),
        ("app.slnx", "<Solution><Project></Solution>"),
        ("main.tf", "resource {broken"),
    ] {
        let f = parse(path, src, "h").unwrap().unwrap();
        assert!(f.nodes.is_empty(), "{path}: {:?}", f.nodes);
        assert!(!f.diagnostics.is_empty(), "{path}");
    }
    assert!(
        !parse(".mcp.json", &" ".repeat(1_048_577), "h")
            .unwrap()
            .unwrap()
            .diagnostics
            .is_empty()
    );
}
#[test]
fn cargo_runtime_dependencies_workspace_and_renames() {
    let f = facts(
        "crates/front/Cargo.toml",
        r#"
[package]
name = "front"
version.workspace = true
[dependencies]
renamed = { package = "engine", path = "../engine" }
shared = { workspace = true }
external = "1"
[dev-dependencies]
test-only = "1"
[build-dependencies]
build-only = "1"
[target.'cfg(unix)'.dependencies]
platform = "1"
"#,
    );
    assert_eq!(
        node(&f, "front").metadata["version"],
        serde_json::Value::Null
    );
    let refs: Vec<_> = f.references.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(refs.len(), 4);
    for expected in ["engine", "shared", "external", "platform"] {
        assert!(refs.contains(&expected));
    }
    assert_eq!(
        f.references
            .iter()
            .find(|r| r.label == "engine")
            .unwrap()
            .candidate_keys,
        ["cargo:manifest:crates/engine/Cargo.toml:engine"]
    );
    let workspace = facts(
        "Cargo.toml",
        "[workspace]\nmembers=['crates/*']\nexclude=['crates/skip']\n[workspace.dependencies]\nshared={package='actual',path='crates/actual'}\n[workspace.package]\nversion='1.0'\n",
    );
    assert!(!workspace.nodes.iter().any(|n| n.kind == "package"));
    assert_eq!(
        workspace.nodes[0].metadata["workspace"]["members"][0],
        "crates/*"
    );
    assert_eq!(
        workspace.nodes[0].metadata["workspace"]["dependencies"]["shared"]["package"],
        "actual"
    );
}
#[test]
fn manifests_preserve_ecosystems_and_deduplicate_dependencies() {
    let py = facts(
        "pyproject.toml",
        "[project]\nname='Demo_App'\nversion='1'\ndependencies=[\"requests[security]>=2; python_version > '3'\", 'Demo-App==1']\n[tool.poetry.dependencies]\npython='^3'\nrequests='*'\n",
    );
    assert_eq!(
        node(&py, "Demo_App").binding_key.as_deref(),
        Some("package:python:demo-app")
    );
    assert_eq!(py.references.len(), 1);
    assert_eq!(py.references[0].label, "requests");
    let apm = facts(
        "agents/apm.yml",
        "name: suite\nversion: '2'\ndependencies:\n - helper\n - other: latest\n",
    );
    assert_eq!(apm.references.len(), 2);
    let go = facts(
        "go.mod",
        "module example.test/demo\nrequire (\n example.test/one v1.0 // indirect\n example.test/two v2.0\n)\nrequire example.test/three v1.0\n",
    );
    assert_eq!(go.references.len(), 3);
    let maven = facts(
        "pom.xml",
        r#"<project xmlns="urn:maven"><groupId>demo</groupId><artifactId>core</artifactId><version>1</version><dependencies><dependency><groupId>demo</groupId><artifactId>util</artifactId></dependency></dependencies></project>"#,
    );
    assert_eq!(
        node(&maven, "demo:core").binding_key.as_deref(),
        Some("package:maven:demo:core")
    );
    assert_eq!(
        maven.references[0].candidate_keys,
        ["package:maven:demo:util"]
    );
}
#[test]
fn named_jsonc_configs_have_real_inheritance_and_distinct_key_paths() {
    let f = facts(
        "web/tsconfig.json",
        r##"{
// preserve string comment markers
"extends": ["./base.json", "external/base"],
"compilerOptions": {"lib": ["dom", "esnext"], "paths": {"@/*": ["src/*"]}},
"a": {"child": {"value": true}}, "b": {"child": {"value": true}},
"definitions": {"record": {}}, "$ref": "#/definitions/record",
"url": "https://example.test/*hello*/",
}"##,
    );
    let inherit: Vec<_> = f
        .references
        .iter()
        .filter(|r| r.relation == "extends")
        .collect();
    assert_eq!(inherit.len(), 2);
    assert!(
        inherit
            .iter()
            .any(|r| r.candidate_keys == ["config:file:web/base.json"])
    );
    assert!(
        !f.references
            .iter()
            .any(|r| ["dom", "esnext"].contains(&r.label.as_str()))
    );
    let keys: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| n.label == "value")
        .map(|n| n.binding_key.clone())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    let package = facts(
        "package.json",
        r#"{"name":"demo","dependencies":{"util":"1"},"bundledDependencies":["bundled"]}"#,
    );
    assert_eq!(package.references.len(), 2);
}
#[test]
fn dotnet_projects_and_solution_dependencies_are_portable() {
    let f = facts(
        "src/app.csproj",
        r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFrameworks>net8.0; net9.0</TargetFrameworks></PropertyGroup><ItemGroup><PackageReference Include="Example.Package"><Version>1.2</Version></PackageReference><ProjectReference Include="..\lib\lib.csproj"/><ProjectReference Include="..\..\outside.csproj"/></ItemGroup></Project>"#,
    );
    for label in ["net8.0", "net9.0", "Example.Package", "Microsoft.NET.Sdk"] {
        node(&f, label);
    }
    assert_eq!(node(&f, "Example.Package").metadata["version"], "1.2");
    assert!(
        f.references
            .iter()
            .any(|r| r.candidate_keys == ["config:file:lib/lib.csproj"])
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.label.contains("outside") && r.candidate_keys.is_empty())
    );
    let slnx = facts(
        "app.slnx",
        r#"<Solution><Folder Name="/apps/"><Project Path="a.csproj"><BuildDependency Project="b.csproj"/></Project></Folder><Project Path="b.csproj"/></Solution>"#,
    );
    assert!(slnx.edges.iter().any(|e| e.source == node(&slnx, "a").id
        && e.target == node(&slnx, "b").id
        && e.relation == "depends_on"));
    let sln = facts(
        "app.sln",
        r#"Project("{type}") = "A", "a.csproj", "{a}"
ProjectSection(ProjectDependencies) = postProject
{b} = {b}
EndProjectSection
EndProject
Project("{type}") = "B", "b.csproj", "{b}"
EndProject
"#,
    );
    assert!(sln.edges.iter().any(|e| e.source == node(&sln, "A").id
        && e.target == node(&sln, "B").id
        && e.relation == "depends_on"));
}
#[test]
fn terraform_addresses_are_directory_scoped_and_strings_are_not_references() {
    let f = facts(
        "infra/main.tf",
        r#"
variable "region" { default = "unused.fake" }
provider "aws" { region = var.region }
resource "aws_vpc" "main" { cidr_block = "10.0.0.0/16" }
resource "aws_instance" "web" {
  depends_on = [aws_vpc.main]
  tags = { region = "${var.region}", fake = "unused.fake" }
  count = 1
  value = count.index
}
data "aws_ami" "latest" { most_recent = true }
locals { image = data.aws_ami.latest.id }
output "host" { value = aws_instance.web.id }
module "child" { source = "./child" }
"#,
    );
    for label in [
        "var.region",
        "provider.aws",
        "aws_vpc.main",
        "aws_instance.web",
        "data.aws_ami.latest",
        "local.image",
        "output.host",
        "module.child",
    ] {
        node(&f, label);
    }
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "depends_on" && r.label == "aws_vpc.main")
    );
    assert!(
        !f.references
            .iter()
            .any(|r| r.label.starts_with("unused") || r.label.starts_with("count"))
    );
    assert_eq!(
        node(&f, "module.child").metadata["module_source"],
        "./child"
    );
    let other = facts(
        "infra/output.tf",
        "output \"region\" { value = var.region }\n",
    );
    assert_eq!(
        other.references[0].candidate_keys[0],
        node(&f, "var.region").binding_key.clone().unwrap()
    );
    let sibling = facts("other/main.tf", "variable \"region\" {}\n");
    assert_ne!(
        node(&f, "var.region").binding_key,
        node(&sibling, "var.region").binding_key
    );
    assert_eq!(facts("prod.tfvars", "region = \"east\"\n").nodes.len(), 1);
}
#[test]
fn terraform_attributes_keep_literal_types_unicode_and_nested_block_ownership() {
    let f = facts(
        "infra/settings.tf",
        r#"
resource "queue" "jobs" {
  enabled = true
  paused = false
  capacity = 73
  offset = -9
  ratio = 0.125
  absent = null
  label = "café 東京 \u03bb \U0001F642"
  escapes = "line\n\t\"quoted\"\\path"
  template_literal = "$${var.name} %%{if true}"
  region = "asia-east1"
  modes = [true, 4, "batch", null, { mode = "quiet" }]
  tags = { café = "東京", "team.name" = "worker", empty = {}, list = [] }
  capacity_hint = /* comment does not become a value */ 81
  lifecycle {
    prevent_destroy = true
  }
}
variable "label" { default = "queue" }
data "image" "base" { current = true }
provider "cloud" { alias = "backup" }
module "jobs" { source = "./workers" }
output "hint" { value = "queue" }
"#,
    );
    let attrs = &node(&f, "queue.jobs").metadata["attributes"];
    assert_eq!(attrs["enabled"], true);
    assert_eq!(attrs["paused"], false);
    assert_eq!(attrs["capacity"].as_u64(), Some(73));
    assert_eq!(attrs["offset"].as_i64(), Some(-9));
    assert_eq!(attrs["ratio"].as_f64(), Some(0.125));
    assert_eq!(attrs.get("absent"), Some(&serde_json::Value::Null));
    assert_eq!(attrs["label"], "café 東京 λ 🙂");
    assert_eq!(attrs["escapes"], "line\n\t\"quoted\"\\path");
    assert_eq!(attrs["template_literal"], "${var.name} %{if true}");
    assert_eq!(attrs["region"], "asia-east1");
    assert_eq!(
        attrs["modes"],
        serde_json::json!([true, 4, "batch", null, {"mode":"quiet"}])
    );
    assert_eq!(
        attrs["tags"],
        serde_json::json!({"café":"東京", "team.name":"worker", "empty":{}, "list":[]})
    );
    assert_eq!(attrs["capacity_hint"], 81);
    assert!(attrs.get("lifecycle").is_none());
    assert!(attrs.get("prevent_destroy").is_none());
    for (label, key, expected) in [
        ("var.label", "default", serde_json::json!("queue")),
        ("data.image.base", "current", serde_json::json!(true)),
        ("provider.cloud", "alias", serde_json::json!("backup")),
        ("module.jobs", "source", serde_json::json!("./workers")),
        ("output.hint", "value", serde_json::json!("queue")),
    ] {
        assert_eq!(node(&f, label).metadata["attributes"][key], expected);
    }
    let restored: FileFacts = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
    assert_eq!(
        node(&restored, "queue.jobs").metadata,
        node(&f, "queue.jobs").metadata
    );
}

#[test]
fn terraform_attributes_leave_expressions_unresolved_and_preserve_reference_owners() {
    let f = facts(
        "infra/main.tf",
        r#"
variable "region" {}
provider "cloud" { alias = "backup" }
resource "queue" "base" {}
resource "queue" "jobs" {
  region = var.region
  quoted = "var.region"
  provider = cloud.backup
  depends_on = [queue.base]
  mixed = ["static", var.region, { selected = queue.base.id }]
  computed = { (var.region) = "computed-key-value" }
  result = file("do-not-open-this-file")
  template = "not-retained-${var.region}"
  transformed = [for item in [queue.base] : item.id]
  arithmetic = 2 + 3
  lifecycle { replace_triggered_by = [queue.base] }
}
module "jobs" {
  source = "./workers"
  input = queue.jobs.id
}
output "result" { value = module.jobs.id }
"#,
    );
    let jobs = node(&f, "queue.jobs");
    let attrs = &jobs.metadata["attributes"];
    assert_eq!(attrs["quoted"], "var.region");
    for value in [
        &attrs["region"],
        &attrs["provider"],
        &attrs["computed"],
        &attrs["result"],
        &attrs["template"],
        &attrs["transformed"],
        &attrs["arithmetic"],
        &attrs["mixed"][1],
        &attrs["mixed"][2]["selected"],
    ] {
        assert_eq!(value["$hcl"], "unresolved");
        assert!(value["kind"].is_string());
    }
    assert_eq!(attrs["mixed"][0], "static");
    for (label, relation) in [
        ("var.region", "references"),
        ("cloud.backup", "references"),
        ("queue.base", "depends_on"),
        ("queue.base", "references"),
    ] {
        assert!(
            f.references
                .iter()
                .any(|r| r.source == jobs.id && r.label == label && r.relation == relation)
        );
    }
    assert!(!f.references.iter().any(|r| r.label.starts_with("item.")));
    assert!(
        f.references
            .iter()
            .any(|r| r.source == node(&f, "module.jobs").id
                && r.relation == "module_source"
                && r.label == "./workers")
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.source == node(&f, "output.result").id
                && r.candidate_keys == ["terraform:module-output:infra:jobs:id"])
    );
    let serialized = serde_json::to_string(&f).unwrap();
    for withheld in [
        "computed-key-value",
        "do-not-open-this-file",
        "not-retained-",
    ] {
        assert!(!serialized.contains(withheld));
    }
}

#[test]
fn terraform_attributes_redact_before_serialization_recursively_and_in_module_sources() {
    // Construct deliberately invalid credential-shaped fixtures; no real key material.
    let synthetic_key = format!("AKIA{}", "0".repeat(16));
    let synthetic_pem = format!(
        "-----BEGIN {}-----\\nfixture-pem-9\\n-----END {}-----",
        "PRIVATE KEY", "PRIVATE KEY"
    );
    let source = r#"
resource "database" "main" {
  name = "ordinary-safe-name"
  PASSWORD = "fixture-password-1"
  apiKey = "fixture-api-2"
  settings = { connection = { client_secret = "fixture-client-3", port = 2345 } }
  replicas = [{ token = "fixture-token-4", region = "west" }, [{ "private-key" = "fixture-key-5" }]]
  encoded = { "pass\u0077ord" = "fixture-escape-6" }
  endpoint = "https://user:fixture-url-7@example.invalid/repo"
  query = "https://example.invalid/repo?token=fixture-query-8"
  certificate = "TEST_PEM_PLACEHOLDER"
  identifier = "TEST_KEY_PLACEHOLDER"
  header = "Bearer fixture-bearer-10"
  payload = "{\"password\": \"fixture-json-11\"}"
  rendered = format("fixture-format-12", var.region)
  password_ref = var.region
  lifecycle { ignored_secret = "fixture-nested-13" }
}
variable "db_password" { default = "fixture-default-14" }
variable "ordinary" {
  sensitive = true
  default = ["fixture-sensitive-15"]
}
output "ordinary" {
  sensitive = true
  value = "fixture-output-16"
}
module "remote" {
  source = "git::https://user:fixture-module-17@example.invalid/repo"
  input = var.region
}
module "safe" { source = "./workers" }
"#
    .replace("TEST_PEM_PLACEHOLDER", &synthetic_pem)
    .replace("TEST_KEY_PLACEHOLDER", &synthetic_key);
    let f = facts("infra/main.tf", &source);
    let attrs = &node(&f, "database.main").metadata["attributes"];
    for key in [
        "PASSWORD",
        "apiKey",
        "endpoint",
        "query",
        "certificate",
        "identifier",
        "header",
        "payload",
        "password_ref",
    ] {
        assert_eq!(attrs[key], "[redacted]", "{key}");
    }
    assert_eq!(
        attrs["settings"]["connection"]["client_secret"],
        "[redacted]"
    );
    assert_eq!(attrs["settings"]["connection"]["port"], 2345);
    assert_eq!(attrs["replicas"][0]["token"], "[redacted]");
    assert_eq!(attrs["replicas"][0]["region"], "west");
    assert_eq!(attrs["replicas"][1][0]["private-key"], "[redacted]");
    assert_eq!(attrs["encoded"]["password"], "[redacted]");
    assert_eq!(attrs["name"], "ordinary-safe-name");
    for (label, key) in [
        ("var.db_password", "default"),
        ("var.ordinary", "default"),
        ("output.ordinary", "value"),
    ] {
        assert_eq!(node(&f, label).metadata["attributes"][key], "[redacted]");
    }
    assert_eq!(
        node(&f, "module.remote").metadata["module_source"],
        "[redacted]"
    );
    assert_eq!(
        node(&f, "module.remote").metadata["attributes"]["source"],
        "[redacted]"
    );
    assert_eq!(
        node(&f, "module.safe").metadata["module_source"],
        "./workers"
    );
    for label in ["database.main", "module.remote"] {
        assert!(
            f.references
                .iter()
                .any(|r| r.source == node(&f, label).id && r.label == "var.region")
        );
    }
    let serialized = serde_json::to_string(&f).unwrap();
    for secret in [
        "fixture-password-1",
        "fixture-api-2",
        "fixture-client-3",
        "fixture-token-4",
        "fixture-key-5",
        "fixture-escape-6",
        "fixture-url-7",
        "fixture-query-8",
        "fixture-pem-9",
        &synthetic_key,
        "fixture-bearer-10",
        "fixture-json-11",
        "fixture-format-12",
        "fixture-nested-13",
        "fixture-default-14",
        "fixture-sensitive-15",
        "fixture-output-16",
        "fixture-module-17",
    ] {
        assert!(!serialized.contains(secret), "secret leaked: {secret}");
    }
}

#[test]
fn terraform_attributes_mark_unsupported_values_without_raw_fallbacks() {
    let deep = format!(
        "{}\"depth-private-value\"{}",
        "[".repeat(35),
        "]".repeat(35)
    );
    let source = format!(
        r#"
resource "queue" "jobs" {{
  large = 18446744073709551616
  duplicate = {{ same = "first-private-value", same = "second-private-value" }}
  text = <<EOT
heredoc-private-value
EOT
  deep = {deep}
}}
"#
    );
    let f = facts("main.tf", &source);
    let attrs = &node(&f, "queue.jobs").metadata["attributes"];
    assert_eq!(attrs["large"]["kind"], "numeric_range");
    assert_eq!(attrs["duplicate"]["kind"], "duplicate_key");
    assert_eq!(attrs["text"]["$hcl"], "unresolved");
    let serialized = serde_json::to_string(&f).unwrap();
    assert!(serialized.contains("depth_limit"));
    for text in [
        "depth-private-value",
        "first-private-value",
        "second-private-value",
        "heredoc-private-value",
    ] {
        assert!(!serialized.contains(text));
    }
    let malformed = parse(
        "broken.tf",
        "resource \"queue\" \"jobs\" { password = \"fixture-broken-value\"",
        "h",
    )
    .unwrap()
    .unwrap();
    assert!(malformed.nodes.is_empty());
    assert!(!malformed.diagnostics.is_empty());
    assert!(
        !serde_json::to_string(&malformed)
            .unwrap()
            .contains("fixture-broken-value")
    );
}

#[test]
fn sql_constraints_indexes_ctes_and_quoted_identifiers() {
    let f = facts(
        "schema.sql",
        r#"
BEGIN;
CREATE TABLE public.accounts (id INT PRIMARY KEY);
CREATE TABLE public.orders (account_id INT REFERENCES public.accounts(id));
CREATE INDEX orders_account ON public.orders(account_id);
CREATE VIEW summary AS WITH scratch AS (SELECT * FROM public.orders) SELECT * FROM scratch JOIN public.accounts ON true;
ALTER TABLE public.orders ADD FOREIGN KEY (account_id) REFERENCES public.accounts(id);
COMMIT;
CREATE TABLE "Dot.Name" (id INT);
CREATE TABLE "Dot"."Name" (id INT);
"#,
    );
    for label in [
        "public.accounts",
        "public.orders",
        "orders_account",
        "summary",
    ] {
        node(&f, label);
    }
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "indexes" && r.label == "public.orders")
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "reads_from" && r.label == "public.accounts")
    );
    assert!(!f.references.iter().any(|r| r.label == "scratch"));
    let keys: Vec<_> = f
        .nodes
        .iter()
        .filter(|n| n.label == "Dot.Name")
        .map(|n| n.binding_key.clone())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    let target = facts("tables.sql", "CREATE TABLE public.accounts (id INT);");
    assert!(f.references.iter().any(|r| {
        r.relation == "references"
            && r.candidate_keys[0]
                == node(&target, "public.accounts")
                    .binding_key
                    .clone()
                    .unwrap()
    }));
}
#[test]
fn sql_recovery_does_not_extract_ddl_from_strings_comments_or_routine_bodies() {
    let f = facts(
        "routines.sql",
        r#"
-- CREATE PROC fake_comment AS SELECT 1;
SELECT 'CREATE PROCEDURE fake_string AS BEGIN END;';
CREATE OR ALTER PROC [dbo].[a]]b] AS BEGIN SELECT 1; END;
CREATE FUNCTION "public"."work"() RETURNS void AS $body$
BEGIN EXECUTE 'CREATE TABLE fake_dynamic (id INT)'; RAISE NOTICE 'hi'; END;
$body$ LANGUAGE plpgsql;
CREATE TABLE after_routine (id INT);
/* CREATE TABLE fake_block (id INT); */
"#,
    );
    assert!(labels(&f).contains(&"dbo.a]b"));
    assert!(labels(&f).contains(&"public.work"));
    assert!(labels(&f).contains(&"after_routine"));
    assert!(!labels(&f).iter().any(|s| s.contains("fake")));
    let f = facts(
        "views.sql",
        "CREATE TABLE t (id INT); CREATE VIEW outer_view AS SELECT * FROM t JOIN (WITH t AS (SELECT 1) SELECT * FROM t) sub ON true; CREATE VIEW hidden AS WITH t AS (SELECT 1) SELECT * FROM t;",
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.source == node(&f, "outer_view").id && r.label == "t")
    );
    assert!(
        !f.references
            .iter()
            .any(|r| r.source == node(&f, "hidden").id && r.label == "t")
    );
}
#[test]
fn mcp_extracts_names_without_secrets_paths_or_raw_arguments() {
    let text = r#"{"mcpServers":{"files":{"command":"/opt/tools/npx","args":["-y","@example/server-files@1.0","/private/data","--token","synthetic-argument-secret"],"env":{"API_TOKEN":"synthetic-env-secret"}},"fetch":{"command":"uvx","args":["mcp-server-fetch"]}}}"#;
    let f = facts(".mcp.json", text);
    for label in [
        "files",
        "npx",
        "@example/server-files",
        "API_TOKEN",
        "mcp-server-fetch",
    ] {
        node(&f, label);
    }
    assert!(f.edges.iter().any(|e| e.relation == "requires_env"));
    let output = format!("{f:?}");
    for hidden in [
        "synthetic-argument-secret",
        "synthetic-env-secret",
        "/private/data",
        "/opt/tools",
    ] {
        assert!(!output.contains(hidden));
    }
    let other = facts("config/mcp.json", text);
    assert_ne!(
        node(&f, "files").binding_key,
        node(&other, "files").binding_key
    );
    assert_eq!(node(&f, "npx").binding_key, node(&other, "npx").binding_key);
    let alternate = facts(
        "mcp_servers.json",
        r#"{"mcp":{"servers":{"api":{"command":"node","args":["--token","scary-mcp","/private/start.js"]}}}}"#,
    );
    assert!(!alternate.nodes.iter().any(|n| n.kind == "mcp_package"));
}

#[test]
fn content_probe_and_xml_entities_keep_normal_inputs_supported() {
    use graf::languages::configs::recognizes;
    assert!(!supports("settings.json"));
    assert!(recognizes(
        "settings.json",
        r#"{"compilerOptions":{"strict":true}}"#
    ));
    assert!(
        parse(
            "settings.json",
            r#"{"compilerOptions":{"strict":true}}"#,
            "h"
        )
        .unwrap()
        .is_some()
    );
    for text in [r#"{"records":[1,2,3]}"#, "[]", "42", "null"] {
        assert!(!recognizes("settings.json", text));
        assert!(parse("settings.json", text, "h").unwrap().is_none());
    }
    let f = facts(
        "app.csproj",
        "<Project><PropertyGroup><TargetFramework>net8&#x2E;0</TargetFramework></PropertyGroup></Project>",
    );
    node(&f, "net8.0");
    let invalid = parse("app.csproj", "<Project>&unknown;</Project>", "h")
        .unwrap()
        .unwrap();
    assert!(invalid.nodes.is_empty());
    assert!(!invalid.diagnostics.is_empty());
}

mod terraform_context {
    use graf::{
        languages::configs::{TerraformContext, parse},
        model::{Coverage, GraphSnapshot, Node},
        store::Store,
    };
    use std::{collections::BTreeMap, fs, path::PathBuf};

    struct Fixture {
        temp: tempfile::TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let f = Self {
                temp: tempfile::tempdir().unwrap(),
            };
            fs::create_dir(f.root()).unwrap();
            f
        }
        fn root(&self) -> PathBuf {
            self.temp.path().join("repo")
        }
        fn write(&self, path: &str, text: &str) {
            let path = self.root().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fn context(&self, paths: &[&str]) -> TerraformContext {
            TerraformContext::discover(
                &self.root(),
                &paths.iter().map(|p| (*p).into()).collect::<Vec<_>>(),
            )
            .unwrap()
        }
        // Exercise the production Store's incremental rebinding with exact supplied
        // membership, independently of the indexer's pending context integration.
        fn update(&self, paths: &[&str]) -> GraphSnapshot {
            let context = self.context(paths);
            let mut store = Store::create(&self.temp.path().join("graph.db")).unwrap();
            let mut old: BTreeMap<_, _> = store
                .file_stamps()
                .unwrap()
                .into_iter()
                .map(|f| (f.path, f.hash))
                .collect();
            let mut changed = vec![];
            for path in paths {
                let source = fs::read_to_string(self.root().join(path)).unwrap();
                let hash = format!(
                    "{}:{}",
                    blake3::hash(source.as_bytes()),
                    context.fingerprint()
                );
                if old.remove(*path).as_ref() == Some(&hash) {
                    continue;
                }
                let mut facts = parse(path, &source, &hash).unwrap().unwrap();
                assert!(
                    facts.diagnostics.is_empty(),
                    "{path}: {:?}",
                    facts.diagnostics
                );
                context.apply(&mut facts);
                let once = (facts.nodes.len(), facts.references.len());
                context.apply(&mut facts);
                assert_eq!(once, (facts.nodes.len(), facts.references.len()));
                changed.push(facts);
            }
            store
                .apply_native(
                    self.root().to_str().unwrap(),
                    changed,
                    old.into_keys().collect(),
                    Coverage::default(),
                )
                .unwrap();
            let graph = store.snapshot().unwrap();
            assert!(
                graph
                    .edges
                    .iter()
                    .all(|e| graph.nodes.iter().any(|n| n.id == e.source)
                        && graph.nodes.iter().any(|n| n.id == e.target))
            );
            graph
        }
    }
    fn find<'a>(graph: &'a GraphSnapshot, file: &str, label: &str) -> &'a Node {
        graph
            .nodes
            .iter()
            .find(|n| n.file == file && n.label == label)
            .unwrap_or_else(|| panic!("missing {file}:{label}"))
    }
    fn linked(graph: &GraphSnapshot, from: (&str, &str), relation: &str, to: (&str, &str)) -> bool {
        let from = &find(graph, from.0, from.1).id;
        let to = &find(graph, to.0, to.1).id;
        graph
            .edges
            .iter()
            .any(|e| &e.source == from && &e.target == to && e.relation == relation)
    }
    fn reachable(graph: &GraphSnapshot, source: &str, target: &str) -> bool {
        let mut seen = std::collections::HashSet::new();
        let mut pending = vec![source];
        while let Some(id) = pending.pop() {
            if id == target {
                return true;
            }
            if seen.insert(id) {
                pending.extend(
                    graph
                        .edges
                        .iter()
                        .filter(|e| e.source == id)
                        .map(|e| e.target.as_str()),
                );
            }
        }
        false
    }

    #[test]
    fn local_modules_reach_resources_and_exact_outputs_without_name_collisions() {
        let f = Fixture::new();
        f.write("env/dev/main.tf", "module \"app\" {\n source = \"../../apps/app\"\n}\nvariable \"app\" {}\noutput \"local\" { value = var.app }\noutput \"result\" { value = module.app.id }\n");
        f.write("env/prod/main.tf", "module \"app\" { source = \"../../apps/other\" }\noutput \"result\" { value = module.app.id }\n");
        f.write("apps/app/main.tf", "module \"base\" { source = \"../../base\" }\noutput \"id\" { value = module.base.id }\n");
        f.write("apps/app/more.tf", "resource \"service\" \"app\" {}\n");
        f.write(
            "apps/app/nested/main.tf",
            "output \"id\" { value = \"nested\" }\n",
        );
        f.write("base/main.tf", "resource \"storage\" \"data\" {}\n");
        f.write(
            "base/outputs.tf",
            "output \"id\" { value = storage.data.id }\n",
        );
        f.write(
            "apps/other/main.tf",
            "output \"id\" { value = \"other\" }\n",
        );
        for directory in ["a-b/app", "a_b/app"] {
            f.write(
                &format!("{directory}/main.tf"),
                "variable \"name\" {}\noutput \"name\" { value = var.name }\n",
            );
        }
        let paths = [
            "env/dev/main.tf",
            "env/prod/main.tf",
            "apps/app/main.tf",
            "apps/app/more.tf",
            "apps/app/nested/main.tf",
            "base/main.tf",
            "base/outputs.tf",
            "apps/other/main.tf",
            "a-b/app/main.tf",
            "a_b/app/main.tf",
        ];
        let graph = f.update(&paths);
        assert!(linked(
            &graph,
            ("env/dev/main.tf", "module.app"),
            "module_source",
            ("apps/app/main.tf", "Terraform module: apps/app")
        ));
        assert!(linked(
            &graph,
            ("apps/app/main.tf", "Terraform module: apps/app"),
            "contains",
            ("apps/app/more.tf", "more.tf")
        ));
        assert!(linked(
            &graph,
            ("env/dev/main.tf", "output.result"),
            "references",
            ("apps/app/main.tf", "output.id")
        ));
        assert!(linked(
            &graph,
            ("env/prod/main.tf", "output.result"),
            "references",
            ("apps/other/main.tf", "output.id")
        ));
        assert!(!linked(
            &graph,
            ("env/dev/main.tf", "output.result"),
            "references",
            ("apps/app/nested/main.tf", "output.id")
        ));
        assert!(linked(
            &graph,
            ("env/dev/main.tf", "output.local"),
            "references",
            ("env/dev/main.tf", "var.app")
        ));
        assert!(!linked(
            &graph,
            ("env/dev/main.tf", "output.local"),
            "references",
            ("env/dev/main.tf", "module.app")
        ));
        assert!(reachable(
            &graph,
            &find(&graph, "env/dev/main.tf", "Terraform module: env/dev").id,
            &find(&graph, "base/main.tf", "storage.data").id
        ));
        let edge = graph
            .edges
            .iter()
            .find(|e| {
                e.source == find(&graph, "env/dev/main.tf", "module.app").id
                    && e.relation == "module_source"
            })
            .unwrap();
        assert_eq!(edge.file.as_deref(), Some("env/dev/main.tf"));
        assert_eq!(edge.line, Some(2));
        for dir in ["a-b/app", "a_b/app"] {
            let path = format!("{dir}/main.tf");
            assert!(linked(
                &graph,
                (&path, "output.name"),
                "references",
                (&path, "var.name")
            ));
        }
        assert_ne!(
            find(&graph, "a-b/app/main.tf", "var.name").binding_key,
            find(&graph, "a_b/app/main.tf", "var.name").binding_key
        );
        assert_eq!(
            f.context(&paths).fingerprint(),
            f.context(&paths.into_iter().rev().collect::<Vec<_>>())
                .fingerprint()
        );
    }

    #[test]
    fn local_source_requires_exact_indexed_tf_directory_and_static_unique_declarations() {
        let f = Fixture::new();
        f.write(
            "main.tf",
            r#"
module "valid" { source = "./real" }
module "escaped" { source = "./$${literal}" }
module "unicode" { source = "./re\u0061l" }
module "ignored" { source = "./ignored" }
module "outside" { source = "../outside" }
module "nested" { source = "./nested" }
module "vars" { source = "./vars" }
module "hcl" { source = "./hcl" }
module "remote" { source = "example/remote/cloud" }
module "dynamic" { source = "./${var.name}" }
module "expression" { source = var.source }
module "computed" { source = "./" + "real" }
module "duplicate" { source = "./real" }
module "duplicate" { source = "./ignored" }
module "duplicate_source" {
 source = "./real"
 source = "./ignored"
}
module "settings" {
 source = "./real"
 settings = { source = "./ignored" }
}
output "missing" { value = module.valid.storage.data }
output "hcl_only" { value = module.valid.phantom }
output "indexed" { value = module.valid[var.name].id }
output "ambiguous_module" { value = module.duplicate.id }
output "ambiguous_output" { value = module.valid.duplicate }
"#,
        );
        f.write("real/main.tf", "resource \"storage\" \"data\" {}\noutput \"id\" { value = storage.data.id }\noutput \"duplicate\" { value = 1 }\n");
        f.write("real/more.tf", "output \"duplicate\" { value = 2 }\n");
        f.write("real/hints.hcl", "output \"phantom\" { value = 1 }\n");
        f.write("${literal}/main.tf", "variable \"value\" {}\n");
        f.write("ignored/main.tf", "variable \"secret\" {}\n");
        f.write("nested/deeper/main.tf", "variable \"value\" {}\n");
        f.write("vars/values.tfvars", "name = \"dev\"\n");
        f.write(
            "hcl/main.hcl",
            "module \"hidden\" { source = \"../real\" }\n",
        );
        fs::create_dir(f.temp.path().join("outside")).unwrap();
        fs::write(
            f.temp.path().join("outside/main.tf"),
            "variable \"external\" {}\n",
        )
        .unwrap();
        let paths = [
            "main.tf",
            "real/main.tf",
            "real/more.tf",
            "real/hints.hcl",
            "${literal}/main.tf",
            "nested/deeper/main.tf",
            "vars/values.tfvars",
            "hcl/main.hcl",
        ];
        let graph = f.update(&paths);
        let module_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.relation == "module_source")
            .collect();
        let sources: std::collections::BTreeSet<_> = module_edges
            .iter()
            .map(|e| {
                graph
                    .nodes
                    .iter()
                    .find(|n| n.id == e.source)
                    .unwrap()
                    .label
                    .as_str()
            })
            .collect();
        assert_eq!(
            sources,
            std::collections::BTreeSet::from([
                "module.valid",
                "module.escaped",
                "module.unicode",
                "module.settings"
            ])
        );
        for output in [
            "output.missing",
            "output.hcl_only",
            "output.indexed",
            "output.ambiguous_module",
            "output.ambiguous_output",
        ] {
            let owner = find(&graph, "main.tf", output);
            assert!(!graph.edges.iter().any(|e| {
                e.source == owner.id
                    && graph
                        .nodes
                        .iter()
                        .any(|n| n.id == e.target && n.file.starts_with("real/"))
            }));
        }
        for directory in ["ignored", "nested", "vars", "hcl"] {
            assert!(
                !graph
                    .nodes
                    .iter()
                    .any(|n| n.label == format!("Terraform module: {directory}"))
            );
        }
        let before = f.context(&paths).fingerprint().to_string();
        f.write(
            "ignored/main.tf",
            "variable \"changed_but_not_indexed\" {}\n",
        );
        f.write("real/hints.hcl", "output \"different\" { value = 2 }\n");
        assert_eq!(before, f.context(&paths).fingerprint());
        f.write("vars/values.tfvars", "name = \"prod\"\n");
        assert_ne!(before, f.context(&paths).fingerprint());
    }

    #[test]
    fn membership_changes_move_anchor_ownership_remove_links_and_rebind_sources() {
        let f = Fixture::new();
        f.write(
            "env/main.tf",
            "module \"app\" { source = \"../app\" }\noutput \"result\" { value = module.app.id }\n",
        );
        f.write("app/a.tf", "variable \"x\" {}\n");
        f.write("app/b.tf", "output \"id\" { value = \"app\" }\n");
        f.write("other/main.tf", "output \"id\" { value = \"other\" }\n");
        let initial = f.update(&["env/main.tf", "app/a.tf", "app/b.tf", "other/main.tf"]);
        let anchor = find(&initial, "app/a.tf", "Terraform module: app")
            .id
            .clone();
        let before = f
            .context(&["env/main.tf", "app/a.tf", "app/b.tf", "other/main.tf"])
            .fingerprint()
            .to_string();
        fs::remove_file(f.root().join("app/a.tf")).unwrap();
        let moved = f.update(&["env/main.tf", "app/b.tf", "other/main.tf"]);
        assert_eq!(find(&moved, "app/b.tf", "Terraform module: app").id, anchor);
        assert_ne!(
            before,
            f.context(&["env/main.tf", "app/b.tf", "other/main.tf"])
                .fingerprint()
        );
        assert!(linked(
            &moved,
            ("env/main.tf", "module.app"),
            "module_source",
            ("app/b.tf", "Terraform module: app")
        ));
        fs::remove_file(f.root().join("app/b.tf")).unwrap();
        let deleted = f.update(&["env/main.tf", "other/main.tf"]);
        assert!(!deleted.edges.iter().any(|e| e.relation == "module_source"));
        assert!(!deleted.nodes.iter().any(|n| n.id == anchor));
        f.write("app/c.tf", "output \"id\" { value = \"new\" }\n");
        let added = f.update(&["env/main.tf", "app/c.tf", "other/main.tf"]);
        assert_eq!(find(&added, "app/c.tf", "Terraform module: app").id, anchor);
        assert!(linked(
            &added,
            ("env/main.tf", "output.result"),
            "references",
            ("app/c.tf", "output.id")
        ));
        // The excluded file still exists: only the approved index membership changes.
        let ignored = f.update(&["env/main.tf", "other/main.tf"]);
        assert!(!ignored.edges.iter().any(|e| e.relation == "module_source"));
        f.write("env/main.tf", "module \"app\" { source = \"../other\" }\noutput \"result\" { value = module.app.id }\n");
        let redirected = f.update(&["env/main.tf", "app/c.tf", "other/main.tf"]);
        assert!(linked(
            &redirected,
            ("env/main.tf", "output.result"),
            "references",
            ("other/main.tf", "output.id")
        ));
        assert!(!linked(
            &redirected,
            ("env/main.tf", "output.result"),
            "references",
            ("app/c.tf", "output.id")
        ));
    }

    #[test]
    fn repeated_calls_cycles_missing_files_and_portable_fingerprints_are_bounded() {
        let a = Fixture::new();
        let b = Fixture::new();
        for f in [&a, &b] {
            f.write(
                "main.tf",
                "module \"a\" { source = \"./child\" }\nmodule \"b\" { source = \"./child\" }\n",
            );
            f.write("child/main.tf", "module \"parent\" { source = \"../\" }\n");
        }
        let paths = ["main.tf", "child/main.tf"];
        assert_eq!(
            a.context(&paths).fingerprint(),
            b.context(&paths).fingerprint()
        );
        let graph = a.update(&paths);
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.relation == "module_source")
                .count(),
            3
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| n.metadata["terraform_directory_anchor"] == true)
                .count(),
            2
        );
        let missing = a.context(&["main.tf", "child/missing.tf"]);
        let mut caller = parse(
            "main.tf",
            &fs::read_to_string(a.root().join("main.tf")).unwrap(),
            "h",
        )
        .unwrap()
        .unwrap();
        missing.apply(&mut caller);
        assert!(
            caller
                .references
                .iter()
                .filter(|r| r.relation == "module_source")
                .all(|r| r.candidate_keys.is_empty())
        );
        for path in ["../outside.tf", "/outside.tf", "child/../main.tf"] {
            assert!(TerraformContext::discover(&a.root(), &[path.into()]).is_err());
        }
    }

    #[test]
    fn credential_assignment_whitespace_never_reaches_facts_or_search() {
        use graf::model::QueryOptions;
        let f = Fixture::new();
        let mut source = String::from("resource \"queue\" \"jobs\" {\n");
        let mut cases = vec![
            ("payload".to_owned(), "fixtureleakmarker".to_owned()),
            ("settings".to_owned(), "fixtureassignmentmarker".to_owned()),
        ];
        source.push_str(
            r#"payload = "{\"password\" : \"fixtureleakmarker\"}"
settings = "password = fixtureassignmentmarker"
ordinary = "region = west"
compact = "region=west"
prose = "password notes = public"
"#,
        );
        for before in ["", " ", "\t", "\n", " \t\r\n"] {
            for after in ["", " ", "\t", "\n", " \t\r\n"] {
                for quoted in [false, true] {
                    let field = format!("field{}", cases.len());
                    let marker = format!("fixturewhitespace{}marker", cases.len());
                    let value = if quoted {
                        format!("{{\"password\"{before}:{after}\"{marker}\"}}")
                    } else {
                        format!("password{before}={after}{marker}")
                    };
                    source.push_str(&format!(
                        "{field} = {}\n",
                        serde_json::to_string(&value).unwrap()
                    ));
                    cases.push((field, marker));
                }
            }
        }
        source.push_str("}\n");
        let facts = super::facts("main.tf", &source);
        let attrs = &super::node(&facts, "queue.jobs").metadata["attributes"];
        let serialized = serde_json::to_string(&facts).unwrap();
        for (field, marker) in &cases {
            assert_eq!(attrs[field], "[redacted]", "{field}");
            assert!(!serialized.contains(marker), "{field} leaked into facts");
        }
        assert_eq!(attrs["ordinary"], "region = west");
        assert_eq!(attrs["compact"], "region=west");
        assert_eq!(attrs["prose"], "password notes = public");

        f.write("main.tf", &source);
        let graph = f.update(&["main.tf"]);
        let persisted = serde_json::to_string(&graph).unwrap();
        let store = Store::open_read_only(&f.temp.path().join("graph.db")).unwrap();
        let options = QueryOptions {
            depth: 0,
            ..Default::default()
        };
        for (field, marker) in &cases {
            assert!(!persisted.contains(marker), "{field} leaked into Store");
            assert!(store.query(marker, &options).unwrap().nodes.is_empty());
        }
        assert!(
            store
                .query("west", &options)
                .unwrap()
                .nodes
                .iter()
                .any(|n| n.label == "queue.jobs")
        );
    }

    #[test]
    fn safe_attribute_search_replaces_old_values_and_never_indexes_credentials() {
        use graf::model::QueryOptions;
        let f = Fixture::new();
        let query = |text: &str| {
            Store::open_read_only(&f.temp.path().join("graph.db"))
                .unwrap()
                .query(
                    text,
                    &QueryOptions {
                        depth: 0,
                        ..Default::default()
                    },
                )
                .unwrap()
        };
        f.write("main.tf", "resource \"queue\" \"jobs\" {\n force_destroy = false\n tags = { purpose = \"catalogqueryold\" }\n password = \"neverindexcredential\"\n}\n");
        f.update(&["main.tf"]);
        for term in ["force_destroy", "catalogqueryold"] {
            let result = query(term);
            let resource = result
                .nodes
                .iter()
                .find(|n| n.label == "queue.jobs")
                .unwrap();
            assert_eq!(resource.metadata["attributes"]["force_destroy"], false);
        }
        assert!(query("neverindexcredential").nodes.is_empty());
        f.write("main.tf", "resource \"queue\" \"jobs\" {\n force_destroy = true\n tags = { purpose = \"catalogquerynew\" }\n}\n");
        f.update(&["main.tf"]);
        assert!(query("catalogqueryold").nodes.is_empty());
        assert!(query("neverindexcredential").nodes.is_empty());
        assert_eq!(
            query("catalogquerynew")
                .nodes
                .iter()
                .find(|n| n.label == "queue.jobs")
                .unwrap()
                .metadata["attributes"]["force_destroy"],
            true
        );
        f.update(&[]);
        assert!(query("catalogquerynew").nodes.is_empty());
        assert!(query("force_destroy").nodes.is_empty());
    }

    #[test]
    fn attributes_round_trip_update_and_delete_without_changing_module_links() {
        let f = Fixture::new();
        f.write("main.tf", "module \"jobs\" { source = \"./workers\" }\noutput \"result\" { value = module.jobs.id }\n");
        f.write("workers/main.tf", "resource \"queue\" \"jobs\" {\n capacity = 17\n password = \"fixture-incremental-secret\"\n tags = { stage = \"initial\" }\n}\noutput \"id\" { value = queue.jobs.id }\n");
        let paths = ["main.tf", "workers/main.tf"];
        let first = f.update(&paths);
        let original = find(&first, "workers/main.tf", "queue.jobs");
        assert_eq!(original.metadata["attributes"]["capacity"], 17);
        assert_eq!(original.metadata["attributes"]["password"], "[redacted]");
        assert!(
            !serde_json::to_string(&first)
                .unwrap()
                .contains("fixture-incremental-secret")
        );
        let unchanged = f.update(&paths);
        assert_eq!(
            find(&unchanged, "workers/main.tf", "queue.jobs").metadata,
            original.metadata
        );

        f.write("workers/main.tf", "resource \"queue\" \"jobs\" {\n capacity = 29\n tags = { stage = \"revised\" }\n}\noutput \"id\" { value = queue.jobs.id }\n");
        let updated = f.update(&paths);
        let resource = find(&updated, "workers/main.tf", "queue.jobs");
        assert_eq!(resource.id, original.id);
        assert_eq!(resource.binding_key, original.binding_key);
        assert_eq!(resource.metadata["attributes"]["capacity"], 29);
        assert_eq!(resource.metadata["attributes"]["tags"]["stage"], "revised");
        assert!(resource.metadata["attributes"].get("password").is_none());
        assert!(linked(
            &updated,
            ("main.tf", "module.jobs"),
            "module_source",
            ("workers/main.tf", "Terraform module: workers")
        ));
        assert!(linked(
            &updated,
            ("main.tf", "output.result"),
            "references",
            ("workers/main.tf", "output.id")
        ));
        let removed = f.update(&["main.tf"]);
        assert!(!removed.nodes.iter().any(|n| n.id == resource.id));
        assert!(!serde_json::to_string(&removed).unwrap().contains("revised"));
    }

    #[cfg(unix)]
    #[test]
    fn indexed_symlinks_cannot_supply_module_targets() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        f.write("main.tf", "module \"linked\" { source = \"./linked\" }\nmodule \"file\" { source = \"./file\" }\n");
        let outside = f.temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("main.tf"), "output \"secret\" { value = 1 }\n").unwrap();
        symlink(&outside, f.root().join("linked")).unwrap();
        fs::create_dir(f.root().join("file")).unwrap();
        symlink(outside.join("main.tf"), f.root().join("file/main.tf")).unwrap();
        let context = f.context(&["main.tf", "linked/main.tf", "file/main.tf"]);
        let mut caller = parse(
            "main.tf",
            &fs::read_to_string(f.root().join("main.tf")).unwrap(),
            "h",
        )
        .unwrap()
        .unwrap();
        context.apply(&mut caller);
        assert!(
            caller
                .references
                .iter()
                .filter(|r| r.relation == "module_source")
                .all(|r| r.candidate_keys.is_empty())
        );
    }
}
