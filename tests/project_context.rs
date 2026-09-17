use graf::{
    index::{self, IndexOptions},
    model::{Direction, GraphSnapshot, Node, QueryOptions},
    store::Store,
};
use std::{collections::BTreeMap, fs};
use tempfile::{TempDir, tempdir};

struct Fixture {
    temp: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let result = Self {
            temp: tempdir().unwrap(),
        };
        fs::create_dir(result.root()).unwrap();
        result
    }
    fn root(&self) -> std::path::PathBuf {
        self.temp.path().join("repo")
    }
    fn db(&self) -> std::path::PathBuf {
        self.temp.path().join("graph.db")
    }
    fn write(&self, path: &str, source: &str) {
        let target = self.root().join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, source).unwrap();
    }
    fn index(&self) -> GraphSnapshot {
        self.index_with(&IndexOptions {
            code_only: true,
            ..Default::default()
        })
    }
    fn index_with(&self, options: &IndexOptions) -> GraphSnapshot {
        index::run_with_options(&self.root(), &self.db(), options).unwrap();
        Store::open(&self.db()).unwrap().snapshot().unwrap()
    }
    fn unresolved(&self, source: &Node, label: &str) -> bool {
        Store::open(&self.db())
            .unwrap()
            .neighbors(
                &source.id,
                &QueryOptions {
                    direction: Direction::Outgoing,
                    relation: Some("calls".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .unresolved
            .iter()
            .any(|r| r.label == label)
    }
}
fn node<'a>(graph: &'a GraphSnapshot, file: &str, label: &str) -> &'a Node {
    graph
        .nodes
        .iter()
        .find(|n| n.file == file && n.label == label)
        .unwrap_or_else(|| panic!("missing {file}:{label}"))
}
fn calls(graph: &GraphSnapshot, from: (&str, &str), to: (&str, &str)) -> bool {
    let source = &node(graph, from.0, from.1).id;
    let target = &node(graph, to.0, to.1).id;
    graph
        .edges
        .iter()
        .any(|e| e.relation == "calls" && &e.source == source && &e.target == target)
}

fn file_relation(
    graph: &GraphSnapshot,
    source: &str,
    relation: &str,
    target: (&str, &str),
) -> bool {
    let target = &node(graph, target.0, target.1).id;
    graph.edges.iter().any(|e| {
        e.relation == relation
            && &e.target == target
            && graph
                .nodes
                .iter()
                .any(|n| n.id == e.source && n.file == source)
    })
}

#[test]
fn go_uses_declared_package_names_excludes_external_tests_and_rebinds_modules() {
    let f = Fixture::new();
    f.write("go.mod", "module example.org/project\n\ngo 1.22\n");
    f.write("main.go", "package main\nimport \"example.org/project/wire/v2\"\nfunc Main() { wire.Read() }\nfunc Shadow(wire interface{ Read() }) { wire.Read() }\n");
    f.write("wire/v2/a.go", "package wire\nfunc Read() {}\n");
    f.write("wire/v2/a_test.go", "package wire_test\nfunc Read() {}\n");
    let graph = f.index();
    assert!(calls(&graph, ("main.go", "Main"), ("wire/v2/a.go", "Read")));
    assert!(!calls(
        &graph,
        ("main.go", "Main"),
        ("wire/v2/a_test.go", "Read")
    ));
    assert!(f.unresolved(node(&graph, "main.go", "Shadow"), "wire.Read"));
    f.write("go.mod", "module other.org/project\n\ngo 1.22\n");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"main.go".into())
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.go", "Main"), "wire.Read"));
    assert!(!calls(
        &graph,
        ("main.go", "Main"),
        ("wire/v2/a.go", "Read")
    ));
}

#[test]
fn go_nested_modules_and_package_owner_deletion_do_not_leave_stale_nodes() {
    let f = Fixture::new();
    f.write("go.mod", "module example.org/root\n");
    f.write("sub/go.mod", "module example.org/child\n");
    f.write("sub/a.go", "package service\nfunc First() {}\n");
    f.write("sub/b.go", "package service\nfunc Work() {}\n");
    f.write(
        "main.go",
        "package main\nimport svc \"example.org/child\"\nfunc Main() { svc.Work() }\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.go", "Main"), ("sub/b.go", "Work")));
    fs::remove_file(f.root().join("sub/a.go")).unwrap();
    let graph = f.index();
    let packages: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.binding_key.as_deref() == Some("go:import-module:example.org/child"))
        .collect();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].file, "sub/b.go");
    assert!(calls(&graph, ("main.go", "Main"), ("sub/b.go", "Work")));
}

#[test]
fn typescript_jsonc_paths_baseurl_extends_and_changes_are_applied() {
    let f = Fixture::new();
    f.write(
        "configs/base.json",
        r#"{ // shared options
      "compilerOptions": {"baseUrl":"..", "paths":{"@core/*":["shared/*"]},},
    }"#,
    );
    f.write(
        "apps/tsconfig.json",
        r#"{"extends":"../configs/base.json"}"#,
    );
    f.write("shared/util.ts", "export function work() {}\n");
    f.write("other/util.ts", "export function work() {}\n");
    f.write("apps/main.ts", "import {work} from '@core/util'; import {work as direct} from 'shared/util'; export function Main() { work(); direct(); }\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("apps/main.ts", "Main"),
        ("shared/util.ts", "work")
    ));
    f.write(
        "configs/base.json",
        r#"{"compilerOptions":{"baseUrl":"..","paths":{"@core/*":["other/*"]}}}"#,
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"apps/main.ts".into())
    );
    let graph = f.index();
    assert!(calls(
        &graph,
        ("apps/main.ts", "Main"),
        ("other/util.ts", "work")
    ));
    assert!(calls(
        &graph,
        ("apps/main.ts", "Main"),
        ("shared/util.ts", "work")
    ));
}

#[test]
fn javascript_workspaces_exports_subpaths_and_explicit_file_dependencies_resolve() {
    let f = Fixture::new();
    f.write(
        "package.json",
        r#"{"private":true,"workspaces":["packages/*"]}"#,
    );
    f.write("packages/lib/package.json", r#"{"name":"@sample/lib","exports":{".":"./src/index.ts","./feature/*":"./src/feature/*.ts"}}"#);
    f.write("packages/lib/src/index.ts", "export function work() {}\n");
    f.write(
        "packages/lib/src/feature/tools.ts",
        "export function tool() {}\n",
    );
    f.write(
        "packages/lib/src/private.ts",
        "export function secret() {}\n",
    );
    f.write(
        "packages/app/package.json",
        r#"{"name":"app","dependencies":{"local":"file:../lib"}}"#,
    );
    f.write("packages/app/main.ts", "import {work} from '@sample/lib'; import {tool} from '@sample/lib/feature/tools'; import {work as local} from 'local'; import {secret} from '@sample/lib/src/private'; export function Main() {work(); tool(); local(); secret();}\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("packages/app/main.ts", "Main"),
        ("packages/lib/src/index.ts", "work")
    ));
    assert!(calls(
        &graph,
        ("packages/app/main.ts", "Main"),
        ("packages/lib/src/feature/tools.ts", "tool")
    ));
    assert!(f.unresolved(node(&graph, "packages/app/main.ts", "Main"), "secret"));
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|e| e.relation == "calls"
                && e.source == node(&graph, "packages/app/main.ts", "Main").id
                && e.target == node(&graph, "packages/lib/src/index.ts", "work").id)
            .count(),
        2
    );
}

#[test]
fn package_names_are_not_guessed_and_duplicate_workspace_names_are_ambiguous() {
    let f = Fixture::new();
    f.write("package.json", r#"{"workspaces":["packages/*"]}"#);
    for dir in ["one", "two"] {
        f.write(
            &format!("packages/{dir}/package.json"),
            r#"{"name":"duplicate","exports":"./index.js"}"#,
        );
        f.write(
            &format!("packages/{dir}/index.js"),
            "export function work() {}\n",
        );
    }
    f.write("main.js", "import {work} from 'duplicate'; import {work as guessed} from 'one'; function Main(){work();guessed();}\n");
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.js", "Main"), "work"));
    assert!(f.unresolved(node(&graph, "main.js", "Main"), "guessed"));
}

#[test]
fn cargo_workspace_dependencies_use_actual_library_aliases_and_public_modules() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers=[\"crates/*\"]\n[workspace.dependencies]\nutility={path=\"crates/toolbox\",package=\"toolbox\"}\n");
    f.write(
        "crates/toolbox/Cargo.toml",
        "[package]\nname=\"toolbox\"\nversion=\"0.1.0\"\n[lib]\nname=\"actual_tool\"\n",
    );
    f.write("crates/toolbox/src/lib.rs", "pub fn work() {} pub(crate) fn restricted() {} pub mod nested; mod secret { pub fn hidden() {} }\n");
    f.write("crates/toolbox/src/nested.rs", "pub fn deep() {}\n");
    f.write("crates/toolbox/src/main.rs", "pub fn only_binary() {}\n");
    f.write(
        "crates/app/Cargo.toml",
        "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\nutility.workspace=true\n",
    );
    f.write("crates/app/src/main.rs", "use utility::{work as run, restricted, only_binary}; use utility::secret::hidden; fn Main(){run(); utility::nested::deep(); restricted(); only_binary(); hidden();}\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("crates/app/src/main.rs", "Main"),
        ("crates/toolbox/src/lib.rs", "work")
    ));
    assert!(calls(
        &graph,
        ("crates/app/src/main.rs", "Main"),
        ("crates/toolbox/src/nested.rs", "deep")
    ));
    for label in ["restricted", "only_binary", "hidden"] {
        assert!(
            f.unresolved(node(&graph, "crates/app/src/main.rs", "Main"), label),
            "{label}"
        );
    }
    let aliases = node(&graph, "crates/toolbox/src/lib.rs", "work").metadata["binding_aliases"]
        .as_array()
        .unwrap();
    assert!(
        aliases
            .iter()
            .any(|a| a == "rust:package:crates/toolbox:actual_tool:work")
    );
    assert!(
        aliases
            .iter()
            .any(|a| a == "symbol:crates/toolbox/src/lib.rs::work")
    );
}

#[test]
fn cargo_path_dependency_change_and_module_membership_changes_rebind() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[workspace]\nmembers=[\"app\",\"left\",\"right\"]\n",
    );
    for dir in ["left", "right"] {
        f.write(
            &format!("{dir}/Cargo.toml"),
            &format!("[package]\nname=\"{dir}\"\nversion=\"0.1.0\"\n"),
        );
        f.write(&format!("{dir}/src/lib.rs"), "pub fn work() {}\n");
    }
    f.write("app/Cargo.toml", "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\nchosen={package=\"left\",path=\"../left\"}\n");
    f.write("app/src/main.rs", "use chosen::work; fn Main(){work();}\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("app/src/main.rs", "Main"),
        ("left/src/lib.rs", "work")
    ));
    f.write("app/Cargo.toml", "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\nchosen={package=\"right\",path=\"../right\"}\n");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"app/src/main.rs".into())
    );
    let graph = f.index();
    assert!(calls(
        &graph,
        ("app/src/main.rs", "Main"),
        ("right/src/lib.rs", "work")
    ));
    assert!(!calls(
        &graph,
        ("app/src/main.rs", "Main"),
        ("left/src/lib.rs", "work")
    ));
}

#[test]
fn config_targets_cannot_escape_repository_and_qualified_document_aliases_survive() {
    let f = Fixture::new();
    fs::write(
        f.temp.path().join("outside.ts"),
        "export function escape() {}\n",
    )
    .unwrap();
    f.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"paths":{"outside":["../outside.ts"]}}}"#,
    );
    f.write(
        "main.ts",
        "import {escape} from 'outside'; function Main(){escape();}\n",
    );
    f.write(
        "src/foo.py",
        "class Widget:\n    def render(self):\n        pass\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "escape"));
    assert!(!graph.nodes.iter().any(|n| n.label == "escape"));
    let aliases = node(&graph, "src/foo.py", "render").metadata["binding_aliases"]
        .as_array()
        .unwrap();
    for key in [
        "symbol:render",
        "symbol:Widget.render",
        "symbol:src/foo.py::Widget.render",
    ] {
        assert!(aliases.iter().any(|v| v == key));
    }
}

#[cfg(unix)]
#[test]
fn config_symlinks_are_rejected_before_following_them() {
    let f = Fixture::new();
    f.write("main.ts", "function Main() {}\n");
    let outside = f.temp.path().join("outside.json");
    fs::write(&outside, "{}").unwrap();
    std::os::unix::fs::symlink(outside, f.root().join("tsconfig.json")).unwrap();
    let error = index::run_with_options(
        &f.root(),
        &f.db(),
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("symlinks"));
}

#[test]
fn invalid_baseurl_does_not_turn_paths_into_repository_relative_matches() {
    let f = Fixture::new();
    f.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":"..","paths":{"target":["lib.ts"]}}}"#,
    );
    f.write("lib.ts", "export function work() {}\n");
    f.write(
        "main.ts",
        "import {work} from 'target'; function Main(){work();}\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "work"));
    f.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":".","paths":{"target":["lib.ts"]}}}"#,
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.ts", "Main"), ("lib.ts", "work")));
}

#[test]
fn javascript_relative_directory_selection_does_not_fall_through_missing_exports() {
    let f = Fixture::new();
    f.write("dir/index.ts", "export function work() {}\n");
    f.write(
        "main.ts",
        "import {work} from './dir'; export function Main(){work();}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.ts", "Main"), ("dir/index.ts", "work")));
    f.write("dir.ts", "export function different() {}\n");
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "work"));
    assert!(!calls(
        &graph,
        ("main.ts", "Main"),
        ("dir/index.ts", "work")
    ));
    f.write("exact.ts", "export function work(){}\n");
    f.write("exact.mjs", "export function different(){}\n");
    f.write(
        "literal.mjs",
        "import {work} from './exact.mjs';function Main(){work();}\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "literal.mjs", "Main"), "work"));
}

#[test]
fn javascript_same_stem_lexical_calls_and_explicit_imports_keep_file_identity() {
    let f = Fixture::new();
    f.write(
        "foo.ts",
        "export function f() { f(); } export {f as alias}; export function own() { f(); }\n",
    );
    f.write(
        "foo.mjs",
        "export function f() { f(); } export {f as alias}; export function own() { f(); }\n",
    );
    f.write("main.mjs", "import {f as typed, alias as typedAlias} from './foo.ts'; import {f as module, alias as moduleAlias} from './foo.mjs'; import * as api from './foo.mjs'; function Main(){typed();typedAlias();module();moduleAlias();api.f();} function Shadow(f){f();}\n");
    let graph = f.index();
    for path in ["foo.ts", "foo.mjs"] {
        let other = if path == "foo.ts" {
            "foo.mjs"
        } else {
            "foo.ts"
        };
        for caller in ["f", "own"] {
            assert!(calls(&graph, (path, caller), (path, "f")));
            assert!(!calls(&graph, (path, caller), (other, "f")));
        }
        for label in ["f", "alias"] {
            assert!(calls(&graph, ("main.mjs", "Main"), (path, label)));
            assert!(
                node(&graph, path, label).metadata["binding_aliases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|key| key == &format!("javascript:file:{path}:{label}"))
            );
        }
        assert!(graph.edges.iter().any(|e| e.relation == "aliases"
            && e.source == node(&graph, path, "alias").id
            && e.target == node(&graph, path, "f").id));
        assert!(!graph.edges.iter().any(|e| e.relation == "aliases"
            && e.source == node(&graph, path, "alias").id
            && e.target == node(&graph, other, "f").id));
    }
    assert!(f.unresolved(node(&graph, "main.mjs", "Shadow"), "f"));
}

#[test]
fn star_reexports_follow_cycles_exclude_default_and_preserve_ambiguity() {
    let f = Fixture::new();
    f.write(
        "a.ts",
        "export function work() {} export default function Default() {}\n",
    );
    f.write("b.ts", "export * from './a'; export * from './c';\n");
    f.write("c.ts", "export * from './b';\n");
    f.write(
        "main.ts",
        "import {work} from './c'; import Default from './c'; function Main(){work();Default();}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.ts", "Main"), ("a.ts", "work")));
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "Default"));
    f.write("other.ts", "export function work() {}\n");
    f.write(
        "b.ts",
        "export * from './a'; export * from './other'; export * from './c';\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "Main"), "work"));
    f.write(
        "b.ts",
        "export * from './a'; export * from './other'; export function work() {}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.ts", "Main"), ("b.ts", "work")));
}
#[test]
fn commonjs_context_links_require_forwarding_and_respects_package_module_type() {
    let f = Fixture::new();
    f.write("package.json", r#"{"dependencies":{"dual":"file:./pkg"}}"#);
    f.write(
        "pkg/package.json",
        r#"{"name":"dual","exports":{"import":"./import.mjs","require":"./require.cjs"}}"#,
    );
    f.write("pkg/import.mjs", "export function work(){}\n");
    f.write("pkg/require.cjs", "exports.work=function work(){};\n");
    f.write(
        "use.cjs",
        "const dual=require('dual');function Use(){dual.work();}\n",
    );
    f.write(
        "use.mjs",
        "import {work} from 'dual';function Use(){work();}\n",
    );
    f.write("lib.cjs", "exports.work=function work(){};\n");
    f.write("barrel.cjs", "module.exports=require('./lib.cjs');\n");
    f.write(
        "main.cjs",
        "const lib=require('./barrel.cjs');function Main(){lib.work();}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("main.cjs", "Main"), ("lib.cjs", "work")));
    assert!(calls(
        &graph,
        ("use.cjs", "Use"),
        ("pkg/require.cjs", "work")
    ));
    assert!(calls(
        &graph,
        ("use.mjs", "Use"),
        ("pkg/import.mjs", "work")
    ));
    f.write("package.json", r#"{"type":"module"}"#);
    f.write(
        "esm.js",
        "const lib=require('./lib.cjs');function ESM(){lib.work();}\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "esm.js", "ESM"), "lib.work"));
    assert!(calls(&graph, ("main.cjs", "Main"), ("lib.cjs", "work")));
}

#[test]
fn node_shebang_uses_actual_path_for_local_require_context() {
    let f = Fixture::new();
    f.write("lib.cjs", "exports.work=function work(){};\n");
    f.write(
        "bin.v1/tool",
        "#!/usr/bin/env node\nconst lib=require('../lib.cjs');function Main(){lib.work();}\n",
    );
    let graph = f.index();
    assert!(calls(&graph, ("bin.v1/tool", "Main"), ("lib.cjs", "work")));
    assert_eq!(node(&graph, "bin.v1/tool", "Main").line, Some(2));
}

#[test]
fn xaml_project_context_refreshes_bindings_from_source_and_literal_namespace() {
    let f = Fixture::new();
    let project = "Shop/App.csproj";
    let vm = "Shop/ViewModels/OrdersViewModel.cs";
    let view = "Shop/Views/OrdersView.xaml";
    f.write(project, "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><RootNamespace>Shop</RootNamespace></PropertyGroup></Project>");
    let source = "using CommunityToolkit.Mvvm.ComponentModel; using CommunityToolkit.Mvvm.Input; namespace Shop.ViewModels; public partial class OrdersViewModel { [ObservableProperty] private string _customerName; [RelayCommand] private void Save() {} }";
    let xaml = "<Window xmlns:prism=\"http://prismlibrary.com/\" prism:ViewModelLocator.AutoWireViewModel=\"True\"><TextBlock Text=\"{Binding CustomerName}\"/><Button Command=\"{Binding SaveCommand}\"/></Window>";
    f.write(vm, source);
    f.write(view, xaml);
    let graph = f.index();
    assert!(file_relation(
        &graph,
        view,
        "view_model",
        (vm, "OrdersViewModel")
    ));
    assert!(file_relation(&graph, view, "binds", (vm, "CustomerName")));
    assert!(file_relation(
        &graph,
        view,
        "binds_command",
        (vm, "SaveCommand")
    ));
    assert_eq!(
        node(&graph, vm, "CustomerName").metadata["generated_by"],
        "CommunityToolkit.Mvvm.ComponentModel.ObservableProperty"
    );

    f.write(vm, &source.replace("_customerName", "_displayName"));
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&view.into())
    );
    let graph = f.index();
    assert!(
        !graph
            .nodes
            .iter()
            .any(|n| n.file == vm && n.label == "CustomerName")
    );
    assert!(!file_relation(&graph, view, "binds", (vm, "DisplayName")));
    let generated = |graph: &GraphSnapshot| -> BTreeMap<String, serde_json::Value> {
        graph
            .nodes
            .iter()
            .filter(|n| n.file == vm && n.metadata["generated_by"].is_string())
            .map(|n| (n.id.clone(), serde_json::to_value(n).unwrap()))
            .collect()
    };
    let generated_before = generated(&graph);
    assert!(!generated_before.is_empty());
    assert_eq!(
        node(&graph, vm, "DisplayName").metadata["generated_by"],
        "CommunityToolkit.Mvvm.ComponentModel.ObservableProperty"
    );
    f.write(view, &xaml.replace("CustomerName", "DisplayName"));
    let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
    assert!(changed.contains(&view.into()));
    // Editing a consumer binding does not change the C# declaration/generator proof.
    assert!(!changed.contains(&vm.into()));
    let graph = f.index();
    assert!(file_relation(&graph, view, "binds", (vm, "DisplayName")));
    assert!(file_relation(
        &graph,
        view,
        "binds_command",
        (vm, "SaveCommand")
    ));
    assert_eq!(generated(&graph), generated_before);

    f.write(
        project,
        "<Project><PropertyGroup><RootNamespace>Other</RootNamespace></PropertyGroup></Project>",
    );
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        view,
        "view_model",
        (vm, "OrdersViewModel")
    ));
    for declaration in [
        "<RootNamespace>$(AssemblyName)</RootNamespace>",
        "<RootNamespace Condition=\"'$(Configuration)' == 'Debug'\">Shop</RootNamespace>",
    ] {
        f.write(
            project,
            &format!("<Project><PropertyGroup>{declaration}</PropertyGroup></Project>"),
        );
        let graph = f.index();
        assert!(!file_relation(
            &graph,
            view,
            "view_model",
            (vm, "OrdersViewModel")
        ));
        assert!(
            !graph
                .nodes
                .iter()
                .any(|n| n.file == vm && n.metadata["generated_by"].is_string())
        );
    }
}

#[test]
fn xaml_project_context_respects_ignored_nested_and_ambiguous_project_scopes() {
    let f = Fixture::new();
    let manifest =
        "<Project><PropertyGroup><RootNamespace>Shop</RootNamespace></PropertyGroup></Project>";
    let source = "namespace Shop.ViewModels; public class OrdersViewModel { public string Title {get;set;} }";
    let xaml = "<Window xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\" x:Class=\"Shop.Views.OrdersView\"><TextBlock Text=\"{Binding Title}\"/></Window>";
    f.write(".grafignore", "Shop/Ignored/\nLoose/*.csproj\n");
    f.write("Shop/App.csproj", manifest);
    f.write("Shop/ViewModels/OrdersViewModel.cs", source);
    f.write("Shop/Views/OrdersView.xaml", xaml);
    f.write("Shop/Nested/Nested.csproj", manifest);
    f.write("Shop/Nested/ViewModels/OrdersViewModel.cs", source);
    f.write("Shop/Nested/Views/OrdersView.xaml", xaml);
    f.write("Shop/Ignored/Bad.csproj", "not XML");
    f.write("Shop/Ignored/ViewModels/OrdersViewModel.cs", source);
    f.write("Loose/Excluded.csproj", manifest);
    f.write("Loose/ViewModels/OrdersViewModel.cs", source);
    f.write("Loose/Views/OrdersView.xaml", xaml);
    let graph = f.index();
    for prefix in ["Shop", "Shop/Nested"] {
        let view = format!("{prefix}/Views/OrdersView.xaml");
        let vm = format!("{prefix}/ViewModels/OrdersViewModel.cs");
        assert!(file_relation(
            &graph,
            &view,
            "view_model",
            (&vm, "OrdersViewModel")
        ));
    }
    assert!(!file_relation(
        &graph,
        "Shop/Views/OrdersView.xaml",
        "view_model",
        (
            "Shop/Nested/ViewModels/OrdersViewModel.cs",
            "OrdersViewModel"
        )
    ));
    assert!(!file_relation(
        &graph,
        "Loose/Views/OrdersView.xaml",
        "view_model",
        ("Loose/ViewModels/OrdersViewModel.cs", "OrdersViewModel")
    ));
    assert!(
        !graph
            .nodes
            .iter()
            .any(|n| n.file.starts_with("Shop/Ignored/"))
    );
    f.write("Shop/Second.csproj", manifest);
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        "Shop/Views/OrdersView.xaml",
        "view_model",
        ("Shop/ViewModels/OrdersViewModel.cs", "OrdersViewModel")
    ));
    assert!(file_relation(
        &graph,
        "Shop/Nested/Views/OrdersView.xaml",
        "view_model",
        (
            "Shop/Nested/ViewModels/OrdersViewModel.cs",
            "OrdersViewModel"
        )
    ));
}

#[test]
fn explicit_native_javascript_imports_do_not_substitute_typescript_siblings() {
    let f = Fixture::new();
    for path in ["foo.js", "foo.ts", "module.mjs", "module.mts"] {
        f.write(path, "export function work() {}\n");
    }
    for path in ["common.cjs", "common.cts"] {
        f.write(path, "exports.work = function work() {};\n");
    }
    f.write("main.mjs", "import {work as js} from './foo.js'; import {work as mjs} from './module.mjs'; function Main(){js();mjs();}\n");
    f.write(
        "main.cjs",
        "const lib=require('./common.cjs');function Main(){lib.work();}\n",
    );
    f.write("typed.ts", "import {work as js} from './foo.js'; import {work as mjs} from './module.mjs'; function Main(){js();mjs();}\n");
    f.write(
        "typed.cts",
        "const lib=require('./common.cjs');function Main(){lib.work();}\n",
    );
    let graph = f.index();
    for (caller, target, wrong) in [
        ("main.mjs", "foo.js", "foo.ts"),
        ("main.mjs", "module.mjs", "module.mts"),
        ("main.cjs", "common.cjs", "common.cts"),
        ("typed.ts", "foo.ts", "foo.js"),
        ("typed.ts", "module.mts", "module.mjs"),
        ("typed.cts", "common.cts", "common.cjs"),
    ] {
        assert!(
            calls(&graph, (caller, "Main"), (target, "work")),
            "{caller} -> {target}"
        );
        assert!(
            !calls(&graph, (caller, "Main"), (wrong, "work")),
            "{caller} -> {wrong}"
        );
    }
}

#[test]
fn non_utf8_csharp_and_xaml_clear_old_facts_without_blocking_unrelated_updates() {
    for project in [false, true] {
        let f = Fixture::new();
        if project {
            f.write("App.csproj", "<Project />");
        }
        f.write("Model.cs", "public class Before {}\n");
        f.write(
            "View.xaml",
            "<Window><TextBlock Name=\"Before\" /></Window>",
        );
        f.write("other.ts", "export function before(){}\n");
        let graph = f.index();
        assert!(graph.nodes.iter().any(|n| n.file == "Model.cs"));
        assert!(graph.nodes.iter().any(|n| n.file == "View.xaml"));
        fs::write(f.root().join("Model.cs"), [0xff, 0xfe]).unwrap();
        fs::write(f.root().join("View.xaml"), [0xff]).unwrap();
        f.write("other.ts", "export function after(){}\n");
        assert!(!index::check_update(&f.root(), &f.db()).unwrap().fresh);
        let report = index::run_with_options(
            &f.root(),
            &f.db(),
            &IndexOptions {
                code_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        for path in ["Model.cs", "View.xaml"] {
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|d| d.file == path && d.message.contains("UTF-8"))
            );
        }
        let graph = Store::open(&f.db()).unwrap().snapshot().unwrap();
        assert!(
            !graph
                .nodes
                .iter()
                .any(|n| matches!(n.file.as_str(), "Model.cs" | "View.xaml"))
        );
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.file == "other.ts" && n.label == "after")
        );
        assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);
        f.write("Model.cs", "public class Recovered {}\n");
        f.write(
            "View.xaml",
            "<Window><TextBlock Name=\"Recovered\" /></Window>",
        );
        let graph = f.index();
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.file == "Model.cs" && n.label == "Recovered")
        );
        assert!(graph.nodes.iter().any(|n| n.file == "View.xaml"));
        assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);
    }
    let f = Fixture::new();
    fs::write(f.root().join("App.csproj"), [0xff]).unwrap();
    f.write("Model.cs", "public class Model {}\n");
    let error = index::run_with_options(
        &f.root(),
        &f.db(),
        &IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("project configuration is not UTF-8"));
}

#[test]
fn inherited_cargo_optional_dependencies_remain_unresolved_until_nonoptional() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[workspace]\nmembers=[\"app\",\"tool\"]\n[workspace.dependencies]\ntool={path=\"tool\"}\n",
    );
    f.write(
        "tool/Cargo.toml",
        "[package]\nname=\"tool\"\nversion=\"0.1.0\"\n",
    );
    f.write("tool/src/lib.rs", "pub fn work(){}\n");
    let manifest = "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\ntool={workspace=true,optional=true}\n";
    f.write("app/Cargo.toml", manifest);
    f.write(
        "app/src/main.rs",
        "use tool::work; fn Main(){work();} fn Direct(){tool::work();}\n",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/main.rs", "Main"), "work"));
    assert!(f.unresolved(node(&graph, "app/src/main.rs", "Direct"), "tool::work"));
    f.write(
        "app/Cargo.toml",
        &manifest.replace("optional=true", "optional=false"),
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"app/src/main.rs".into())
    );
    let graph = f.index();
    for caller in ["Main", "Direct"] {
        assert!(calls(
            &graph,
            ("app/src/main.rs", caller),
            ("tool/src/lib.rs", "work")
        ));
    }
    f.write("app/Cargo.toml", manifest);
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/main.rs", "Main"), "work"));
    assert!(!calls(
        &graph,
        ("app/src/main.rs", "Main"),
        ("tool/src/lib.rs", "work")
    ));
    f.write(
        "app/Cargo.toml",
        &manifest.replace("workspace=true", "path=\"../tool\""),
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/main.rs", "Direct"), "tool::work"));
}

#[test]
fn configured_swift_modules_use_exact_roots_imports_and_nested_ownership() {
    let f = Fixture::new();
    f.write("Sources/Library/a.swift", "public func work() {}\n");
    f.write("Sources/Library/b.swift", "func run() { work() }\n");
    f.write(
        "Sources/Library/Nested/n.swift",
        "public func work() {}\nfunc own() { work() }\n",
    );
    f.write("Sources/App/main.swift", "import Core\nimport External\nfunc main() { work(); Core.work(); External.missing() }\nfunc shadow(work: () -> Void) { work() }\n");
    f.write(
        "Loose/tool.swift",
        "import Core\nfunc loose() { Core.work() }\n",
    );
    f.write("Other/Library/a.swift", "public func work() {}\n");
    let mut options = IndexOptions {
        code_only: true,
        swift_modules: BTreeMap::from([
            ("Core".into(), "Sources/Library".into()),
            ("Nested".into(), "Sources/Library/Nested".into()),
            ("App".into(), "Sources/App".into()),
        ]),
        ..Default::default()
    };
    let graph = f.index_with(&options);
    for caller in [
        ("Sources/Library/b.swift", "run"),
        ("Sources/App/main.swift", "main"),
    ] {
        assert!(calls(&graph, caller, ("Sources/Library/a.swift", "work")));
        assert!(!calls(
            &graph,
            caller,
            ("Sources/Library/Nested/n.swift", "work")
        ));
        assert!(!calls(&graph, caller, ("Other/Library/a.swift", "work")));
    }
    assert!(calls(
        &graph,
        ("Sources/Library/Nested/n.swift", "own"),
        ("Sources/Library/Nested/n.swift", "work")
    ));
    assert!(f.unresolved(
        node(&graph, "Sources/App/main.swift", "main"),
        "External.missing"
    ));
    assert!(f.unresolved(node(&graph, "Sources/App/main.swift", "shadow"), "work"));
    assert!(f.unresolved(node(&graph, "Loose/tool.swift", "loose"), "Core.work"));
    assert_eq!(
        index::stored_options(&f.db()).unwrap().swift_modules,
        options.swift_modules
    );
    assert!(index::check_update(&f.root(), &f.db()).unwrap().fresh);

    options
        .swift_modules
        .insert("Core".into(), "Other/Library".into());
    let graph = f.index_with(&options);
    assert!(calls(
        &graph,
        ("Sources/App/main.swift", "main"),
        ("Other/Library/a.swift", "work")
    ));
    assert!(!calls(
        &graph,
        ("Sources/App/main.swift", "main"),
        ("Sources/Library/a.swift", "work")
    ));
    f.write("Other/Library/new.swift", "public func added() {}\n");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .contains(&"Sources/App/main.swift".into())
    );
    f.index_with(&options);
    fs::remove_file(f.root().join("Other/Library/a.swift")).unwrap();
    let graph = f.index_with(&options);
    assert!(f.unresolved(node(&graph, "Sources/App/main.swift", "main"), "Core.work"));
}

#[test]
fn swift_duplicate_roots_and_escaping_configuration_do_not_guess_modules() {
    let f = Fixture::new();
    f.write("src/a.swift", "public func work() {}\n");
    f.write("src/b.swift", "func run() { work() }\n");
    let mut options = IndexOptions {
        code_only: true,
        swift_modules: BTreeMap::from([("One".into(), "src".into()), ("Two".into(), "src".into())]),
        ..Default::default()
    };
    let graph = f.index_with(&options);
    assert!(f.unresolved(node(&graph, "src/b.swift", "run"), "work"));
    assert!(!calls(
        &graph,
        ("src/b.swift", "run"),
        ("src/a.swift", "work")
    ));
    options.swift_modules.remove("Two");
    let graph = f.index_with(&options);
    assert!(calls(
        &graph,
        ("src/b.swift", "run"),
        ("src/a.swift", "work")
    ));
    for root in ["../outside", "/absolute", "src/../outside", "src\\nested"] {
        options.swift_modules.insert("One".into(), root.into());
        assert!(
            index::run_with_options(&f.root(), &f.db(), &options).is_err(),
            "{root}"
        );
    }
}

#[test]
fn configured_component_and_namespace_type_references_use_exact_files() {
    let f = Fixture::new();
    f.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":".","paths":{"@ui/*":["ui/*"],"@types":["types.ts"]}}}"#,
    );
    f.write("ui/Card.vue", "<template><div>Card</div></template>");
    f.write("ui/Card.ts", "export default function Card() {}");
    f.write(
        "types.ts",
        "export interface Shape {} export namespace API { export function run() {} }",
    );
    f.write("app.tsx", "import Card from '@ui/Card.vue'; import type {Shape} from '@types'; import {API} from '@types'; export interface Local extends Shape {} export function App() { API.run(); return <Card/>; } function Shadow(Card: unknown) { return <Card/>; }");
    let graph = f.index();
    assert!(file_relation(
        &graph,
        "app.tsx",
        "uses_component",
        ("ui/Card.vue", "Card")
    ));
    assert!(!file_relation(
        &graph,
        "app.tsx",
        "uses_component",
        ("ui/Card.ts", "Card")
    ));
    assert!(file_relation(
        &graph,
        "app.tsx",
        "inherits",
        ("types.ts", "Shape")
    ));
    assert!(calls(&graph, ("app.tsx", "App"), ("types.ts", "run")));
    assert!(!graph.edges.iter().any(
        |e| e.relation == "uses_component" && e.source == node(&graph, "app.tsx", "Shadow").id
    ));
    fs::remove_file(f.root().join("ui/Card.vue")).unwrap();
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        "app.tsx",
        "uses_component",
        ("ui/Card.ts", "Card")
    ));
}

#[test]
fn go_explicit_imported_receivers_types_and_embeds_respect_export_visibility() {
    let f = Fixture::new();
    f.write("go.mod", "module example.org/app\n\ngo 1.22\n");
    f.write("wire/v2/api.go", "package transport\ntype Packet struct {}\nfunc (p Packet) Send() {}\nfunc (p Packet) hidden() {}\n");
    f.write("main.go", "package main\nimport \"example.org/app/wire/v2\"\ntype Wrapped struct { transport.Packet }\nfunc Send(p transport.Packet) { p.Send(); p.hidden() }\nfunc Shadow(transport interface{ Send() }) { transport.Send() }\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("main.go", "Send"),
        ("wire/v2/api.go", "Send")
    ));
    assert!(!calls(
        &graph,
        ("main.go", "Send"),
        ("wire/v2/api.go", "hidden")
    ));
    assert!(file_relation(
        &graph,
        "main.go",
        "embeds",
        ("wire/v2/api.go", "Packet")
    ));
    assert!(file_relation(
        &graph,
        "main.go",
        "parameter_type",
        ("wire/v2/api.go", "Packet")
    ));
    assert!(f.unresolved(node(&graph, "main.go", "Send"), "p.hidden"));
    assert!(f.unresolved(node(&graph, "main.go", "Shadow"), "transport.Send"));
}

#[test]
fn rust_split_impls_and_public_use_forwarding_refresh_on_source_change() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n");
    f.write(
        "core/Cargo.toml",
        "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n",
    );
    f.write("core/src/lib.rs", "mod state; mod apply; mod save; pub use crate::state::State as Engine; pub use crate::apply::start;");
    f.write("core/src/state.rs", "pub struct State;");
    f.write("core/src/apply.rs", "use crate::state::State; impl State { pub fn apply(&self) { self.save(); } } pub fn start() {} ");
    f.write(
        "core/src/save.rs",
        "use crate::state::State; impl State { pub fn save(&self) {} } ",
    );
    f.write(
        "core/src/orphan.rs",
        "use crate::state::State; impl State { pub fn ghost(&self) {} }",
    );
    f.write("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nengine = { path = \"../core\" }\n");
    f.write("app/src/lib.rs", "use engine::Engine; pub fn run() { Engine::apply(); Engine::save(); Engine::ghost(); engine::start(); }");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("core/src/apply.rs", "apply"),
        ("core/src/save.rs", "save")
    ));
    assert!(calls(
        &graph,
        ("app/src/lib.rs", "run"),
        ("core/src/apply.rs", "apply")
    ));
    assert!(calls(
        &graph,
        ("app/src/lib.rs", "run"),
        ("core/src/save.rs", "save")
    ));
    assert!(calls(
        &graph,
        ("app/src/lib.rs", "run"),
        ("core/src/apply.rs", "start")
    ));
    assert!(!calls(
        &graph,
        ("app/src/lib.rs", "run"),
        ("core/src/orphan.rs", "ghost")
    ));
    f.write("core/src/lib.rs", "mod state; mod apply; mod save; pub use crate::state::State as Renamed; pub use crate::apply::start;");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "app/src/lib.rs")
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/lib.rs", "run"), "Engine::apply"));
    assert!(calls(
        &graph,
        ("core/src/apply.rs", "apply"),
        ("core/src/save.rs", "save")
    ));
}

#[test]
fn rust_public_use_never_selects_ambiguous_private_or_optional_targets() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n");
    f.write(
        "core/Cargo.toml",
        "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n",
    );
    f.write("core/src/lib.rs", "mod a; mod b; pub use crate::a::run as duplicate; pub use crate::b::run as duplicate; pub use crate::a::secret; pub use crate::a::run as okay;");
    f.write("core/src/a.rs", "pub fn run() {} fn secret() {}");
    f.write("core/src/b.rs", "pub fn run() {}");
    f.write("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nengine = { path = \"../core\" }\n");
    f.write(
        "app/src/lib.rs",
        "pub fn call() { engine::duplicate(); engine::secret(); engine::okay(); }",
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/lib.rs", "call"), "engine::duplicate"));
    assert!(f.unresolved(node(&graph, "app/src/lib.rs", "call"), "engine::secret"));
    assert!(calls(
        &graph,
        ("app/src/lib.rs", "call"),
        ("core/src/a.rs", "run")
    ));
    f.write("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nengine = { path = \"../core\", optional = true }\n");
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "app/src/lib.rs", "call"), "engine::okay"));
}

#[test]
fn terraform_context_is_used_by_index_and_refreshes_local_module_outputs() {
    let f = Fixture::new();
    f.write(
        "env/main.tf",
        "module \"app\" { source = \"../app\" }\noutput \"result\" { value = module.app.id }\n",
    );
    f.write("app/main.tf", "output \"id\" { value = \"first\" }\n");
    f.write("other/main.tf", "output \"id\" { value = \"other\" }\n");
    // `env` is excluded as generated content unless the caller opts in.
    let excluded = f.index();
    assert!(!excluded.nodes.iter().any(|n| n.file == "env/main.tf"));
    let options = IndexOptions {
        code_only: true,
        include_generated: true,
        ..Default::default()
    };
    let graph = f.index_with(&options);
    assert!(graph.nodes.iter().any(|n| n.file == "env/main.tf"));
    assert!(file_relation(
        &graph,
        "env/main.tf",
        "module_source",
        ("app/main.tf", "Terraform module: app")
    ));
    assert!(file_relation(
        &graph,
        "env/main.tf",
        "references",
        ("app/main.tf", "output.id")
    ));
    f.write(
        "app/main.tf",
        "output \"renamed\" { value = \"changed\" }\n",
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "env/main.tf")
    );
    let graph = f.index_with(&options);
    assert!(!file_relation(
        &graph,
        "env/main.tf",
        "references",
        ("app/main.tf", "output.renamed")
    ));
    assert!(!file_relation(
        &graph,
        "env/main.tf",
        "references",
        ("other/main.tf", "output.id")
    ));
    f.write(
        "env/main.tf",
        "module \"app\" { source = \"../other\" }\noutput \"result\" { value = module.app.id }\n",
    );
    let graph = f.index_with(&options);
    assert!(file_relation(
        &graph,
        "env/main.tf",
        "references",
        ("other/main.tf", "output.id")
    ));
}

#[test]
fn cargo_package_graph_refreshes_unchanged_manifests_on_add_change_and_delete() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n");
    f.write("app/Cargo.toml", "[package]\nname=\"app\"\nversion=\"0.1.0\"\n[dependencies]\ncore={package=\"library\",path=\"../lib\"}\n");
    let graph = f.index();
    assert!(!graph.edges.iter().any(|e| e.relation == "crate_depends_on"));
    f.write(
        "lib/Cargo.toml",
        "[package]\nname=\"library\"\nversion=\"0.1.0\"\n",
    );
    let refresh = || {
        assert!(
            index::check_update(&f.root(), &f.db())
                .unwrap()
                .changed
                .iter()
                .any(|p| p == "app/Cargo.toml")
        )
    };
    refresh();
    let graph = f.index();
    for relation in ["depends_on", "crate_depends_on"] {
        assert!(file_relation(
            &graph,
            "app/Cargo.toml",
            relation,
            ("lib/Cargo.toml", "library")
        ));
    }
    assert!(graph.edges.iter().any(|e| e.relation == "crate_depends_on"
        && e.metadata["alias"] == "core"
        && e.metadata["context"] == "cargo_dependency"));
    f.write(
        "lib/Cargo.toml",
        "[package]\nname=\"renamed\"\nversion=\"0.1.0\"\n",
    );
    refresh();
    let graph = f.index();
    assert!(!graph.edges.iter().any(|e| e.relation == "crate_depends_on"));
    f.write(
        "lib/Cargo.toml",
        "[package]\nname=\"library\"\nversion=\"0.1.0\"\n",
    );
    f.index();
    fs::remove_file(f.root().join("lib/Cargo.toml")).unwrap();
    refresh();
    let graph = f.index();
    assert!(!graph.nodes.iter().any(|n| n.file == "lib/Cargo.toml"));
    assert!(!graph.edges.iter().any(|e| e.relation == "crate_depends_on"));
}

#[test]
fn javascript_receiver_calls_resolve_exact_files_and_refresh_changed_members() {
    let f = Fixture::new();
    f.write("service.ts", "export class Service { run() {} static create() {} private hidden() {} } export function local() { const x = new Service(); x.run(); }");
    f.write("service.mjs", "export class Service { run() {} static create() {} } export function local() { const x = new Service(); x.run(); }");
    f.write(
        "default.ts",
        "export default class Hidden { forbidden() {} }",
    );
    f.write("barrel.ts", "export * from './default';");
    f.write(
        "main.ts",
        r#"
import {Service as Typed} from './service.ts';
import {Service as Native} from './service.mjs';
import type {Service as External} from 'external';
import Hidden from './barrel';
export function noDefault() { const x = new Hidden(); x.forbidden(); }
export function typed(x: Typed) { x.run(); x.hidden(); Typed.create(); }
export function native() { const x = new Native(); x.run(); }
export function external(x: External) { x.run(); }
export function untyped(x) { x.run(); }
"#,
    );
    let graph = f.index();
    for (from, to) in [
        (("service.ts", "local"), ("service.ts", "run")),
        (("service.mjs", "local"), ("service.mjs", "run")),
        (("main.ts", "typed"), ("service.ts", "run")),
        (("main.ts", "typed"), ("service.ts", "create")),
        (("main.ts", "native"), ("service.mjs", "run")),
    ] {
        assert!(calls(&graph, from, to), "{from:?} -> {to:?}");
    }
    assert!(!calls(
        &graph,
        ("service.ts", "local"),
        ("service.mjs", "run")
    ));
    assert!(!calls(
        &graph,
        ("service.mjs", "local"),
        ("service.ts", "run")
    ));
    for owner in ["external", "untyped"] {
        assert!(f.unresolved(node(&graph, "main.ts", owner), "x.run"));
    }
    assert!(f.unresolved(node(&graph, "main.ts", "typed"), "x.hidden"));
    assert!(f.unresolved(node(&graph, "main.ts", "noDefault"), "x.forbidden"));
    f.write(
        "service.ts",
        "export class Service { renamed() {} static create() {} private hidden() {} }",
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "main.ts")
    );
    let graph = f.index();
    assert!(f.unresolved(node(&graph, "main.ts", "typed"), "x.run"));
    assert!(calls(&graph, ("main.ts", "native"), ("service.mjs", "run")));
}

#[test]
fn javascript_citations_exist_without_documents_and_update_without_stale_edges() {
    let f = Fixture::new();
    f.write(
        "main.ts",
        "export function run() {\n // WHY: ADR12 and rfc2119\n}\n",
    );
    let graph = f.index();
    let citation = node(&graph, "main.ts", "ADR-0012");
    assert_eq!(citation.kind, "doc_ref");
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.relation == "cites" && e.target == citation.id)
    );
    assert!(graph.nodes.iter().all(|n| n.file == "main.ts"));
    f.write("main.ts", "export function run() {\n // TODO: RFC 822\n}\n");
    let graph = f.index();
    assert!(
        !graph
            .nodes
            .iter()
            .any(|n| n.label == "ADR-0012" || n.label == "RFC-2119")
    );
    let citation = node(&graph, "main.ts", "RFC-822");
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.relation == "cites" && e.target == citation.id)
    );
}

#[test]
fn rust_constants_follow_public_use_and_actual_module_boundaries() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname=\"library\"\nversion=\"0.1.0\"\n",
    );
    f.write("src/lib.rs", "mod values; pub use crate::values::DEFAULT as DEFAULT_VALUE; pub use crate::values::Value as PublicValue;");
    f.write("src/values.rs", "pub struct Value; pub const DEFAULT: Value = Value; impl Value { pub const EMPTY: Self = Value; const PRIVATE: Self = Value; }");
    f.write(
        "src/orphan.rs",
        "use crate::values::Value; impl Value { pub const ORPHAN: Self = Value; }",
    );
    let graph = f.index();
    let aliases = |label: &str| {
        node(&graph, "src/values.rs", label).metadata["binding_aliases"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert!(
        aliases("DEFAULT")
            .iter()
            .any(|k| k.ends_with(":DEFAULT_VALUE"))
    );
    assert!(
        aliases("EMPTY")
            .iter()
            .any(|k| k.ends_with(":PublicValue::EMPTY"))
    );
    assert!(
        !node(&graph, "src/values.rs", "PRIVATE").metadata["binding_aliases"]
            .as_array()
            .is_some_and(|keys| keys.iter().any(|k| k
                .as_str()
                .is_some_and(|k| k.ends_with(":PublicValue::PRIVATE"))))
    );
    assert!(
        node(&graph, "src/orphan.rs", "ORPHAN")
            .binding_key
            .is_none()
    );
    f.write("src/lib.rs", "mod values; pub use crate::values::DEFAULT as CHANGED; pub use crate::values::Value as PublicValue;");
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "src/values.rs")
    );
    let graph = f.index();
    let values = node(&graph, "src/values.rs", "DEFAULT").metadata["binding_aliases"]
        .as_array()
        .unwrap();
    assert!(
        values
            .iter()
            .any(|v| v.as_str().is_some_and(|k| k.ends_with(":CHANGED")))
    );
    assert!(
        !values
            .iter()
            .any(|v| v.as_str().is_some_and(|k| k.ends_with(":DEFAULT_VALUE")))
    );
}

#[test]
fn compiled_csharp_project_units_refresh_proof_but_ignore_body_comments() {
    let f = Fixture::new();
    f.write(
        "App/App.csproj",
        "<Project><PropertyGroup><RootNamespace>api</RootNamespace></PropertyGroup></Project>",
    );
    let base = "namespace api; public class Base { public void work() {} }";
    f.write("App/Base.cs", base);
    f.write(
        "App/Derived.cs",
        "namespace api; public class Derived : Base {}",
    );
    f.write(
        "App/Run.cs",
        "namespace api; class Run { void run(Derived value) { value.work(); } }",
    );
    f.write(
        "App/View.xaml",
        "<Window xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\" x:Class=\"api.View\"/>",
    );
    f.write("Other/Other.csproj", "<Project/>");
    f.write("Other/Base.cs", base);
    let graph = f.index();
    assert!(calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
    assert!(!calls(
        &graph,
        ("App/Run.cs", "run"),
        ("Other/Base.cs", "work")
    ));
    f.write(
        "App/Base.cs",
        "namespace api; public class Base { public void work() { /* explanation */ } }",
    );
    let changed = index::check_update(&f.root(), &f.db()).unwrap().changed;
    assert!(changed.iter().any(|p| p == "App/Base.cs"));
    assert!(changed.iter().any(|p| p == "App/View.xaml"));
    assert!(
        !changed
            .iter()
            .any(|p| p == "App/Run.cs" || p == "Other/Base.cs")
    );
    let graph = f.index();
    assert!(calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
    f.write(
        "App/Base.cs",
        "namespace api; public class Base { private void work() {} }",
    );
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "App/Run.cs")
    );
    let graph = f.index();
    assert!(!calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
    f.write("App/Base.cs", base);
    f.write("App/Ambiguous.csproj", "<Project/>");
    let graph = f.index();
    assert!(!calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
    fs::remove_file(f.root().join("App/Ambiguous.csproj")).unwrap();
    let graph = f.index();
    assert!(calls(
        &graph,
        ("App/Run.cs", "run"),
        ("App/Base.cs", "work")
    ));
}

#[test]
fn compiled_root_analysis_units_stop_at_indexed_split_markers() {
    for (extension, base, derived, caller, marker, manifest) in [
        (
            "java",
            "package api; public class Base { public final void work() {} }",
            "package api; public class Derived extends Base {}",
            "package api; class Run { void run(Derived value) { value.work(); } }",
            "nested/pom.xml",
            "<project><modelVersion>4.0.0</modelVersion><groupId>sample</groupId><artifactId>nested</artifactId><version>1</version></project>",
        ),
        (
            "kt",
            "package api\nopen class Base {\n fun work() {}\n}\n",
            "package api\nclass Derived : Base()\n",
            "package api\nfun run(value: Derived) { value.work() }\n",
            "nested/build.gradle.kts",
            "",
        ),
        (
            "cpp",
            "namespace api { struct Base { void work() {} }; }",
            "namespace api { struct Derived : Base {}; }",
            "namespace api { void run(Derived value) { value.work(); } }",
            "nested/CMakeLists.txt",
            "project(nested)\n",
        ),
    ] {
        let f = Fixture::new();
        let base_path = format!("Base.{extension}");
        let derived_path = format!("Derived.{extension}");
        let run_path = format!("Run.{extension}");
        f.write(&base_path, base);
        f.write(&derived_path, derived);
        f.write(&run_path, caller);
        // Plain CMake text participates in normal indexing, not code-only mode.
        let options = IndexOptions {
            code_only: extension != "cpp",
            ..Default::default()
        };
        let graph = f.index_with(&options);
        assert!(
            calls(&graph, (&run_path, "run"), (&base_path, "work")),
            "{extension}"
        );
        f.write(marker, manifest);
        assert!(
            index::check_update(&f.root(), &f.db())
                .unwrap()
                .changed
                .contains(&run_path)
        );
        let graph = f.index_with(&options);
        assert!(
            !calls(&graph, (&run_path, "run"), (&base_path, "work")),
            "{extension}"
        );
        fs::remove_file(f.root().join(marker)).unwrap();
        assert!(
            index::check_update(&f.root(), &f.db())
                .unwrap()
                .changed
                .contains(&run_path)
        );
        let graph = f.index_with(&options);
        assert!(
            calls(&graph, (&run_path, "run"), (&base_path, "work")),
            "{extension}"
        );
    }
}

#[test]
fn extended_pascal_context_refreshes_unchanged_callers_and_removes_deleted_targets() {
    let f = Fixture::new();
    let base = "unit Foundation;\ninterface\ntype TBase = class\n procedure Prepare;\nend;\nimplementation\nprocedure TBase.Prepare; begin end;\nend.\n";
    f.write("Foundation.pas", base);
    f.write("Child.pas", "unit Child;\ninterface\nuses Foundation;\ntype TChild = class(TBase)\n procedure Run;\nend;\nimplementation\nprocedure TChild.Run; begin inherited Prepare; end;\nend.\n");
    let graph = f.index();
    assert!(calls(
        &graph,
        ("Child.pas", "tchild.run"),
        ("Foundation.pas", "tbase.prepare")
    ));
    f.write("Foundation.pas", &base.replace("Prepare", "Changed"));
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "Child.pas")
    );
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        "Child.pas",
        "calls",
        ("Foundation.pas", "tbase.changed")
    ));
    f.write("Foundation.pas", base);
    f.index();
    fs::remove_file(f.root().join("Foundation.pas")).unwrap();
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "Child.pas")
    );
    let graph = f.index();
    assert!(!graph.nodes.iter().any(|n| n.file == "Foundation.pas"));
    assert!(!graph.edges.iter().any(|e| e.relation == "calls"));
}

#[test]
fn razor_index_uses_loose_or_nearest_project_types_and_refreshes_on_removal() {
    let f = Fixture::new();
    let source = "namespace Demo; public class Service {}";
    f.write("Service.cs", source);
    f.write("Page.razor", "@using Demo\n@inject Service service\n");
    f.write("View.cshtml", "@inject Demo.Service service\n");
    let graph = f.index();
    for page in ["Page.razor", "View.cshtml"] {
        assert!(file_relation(
            &graph,
            page,
            "uses_type",
            ("Service.cs", "Service")
        ));
    }
    f.write("Nested/App.csproj", "<Project/>");
    f.write("Nested/Service.cs", source);
    f.write("Nested/Page.razor", "@inject Demo.Service service\n");
    let graph = f.index();
    assert!(file_relation(
        &graph,
        "Nested/Page.razor",
        "uses_type",
        ("Nested/Service.cs", "Service")
    ));
    assert!(file_relation(
        &graph,
        "Page.razor",
        "uses_type",
        ("Service.cs", "Service")
    ));
    assert!(!file_relation(
        &graph,
        "Page.razor",
        "uses_type",
        ("Nested/Service.cs", "Service")
    ));
    f.write("Nested/Other.csproj", "<Project/>");
    let graph = f.index();
    for provider in ["Service.cs", "Nested/Service.cs"] {
        assert!(!file_relation(
            &graph,
            "Nested/Page.razor",
            "uses_type",
            (provider, "Service")
        ));
    }
    fs::remove_file(f.root().join("Nested/Other.csproj")).unwrap();
    f.write("Nested/App.csproj", "<Project><PropertyGroup><RootNamespace>$(Unknown)</RootNamespace></PropertyGroup></Project>");
    let graph = f.index();
    assert!(!file_relation(
        &graph,
        "Nested/Page.razor",
        "uses_type",
        ("Nested/Service.cs", "Service")
    ));
    fs::remove_file(f.root().join("Service.cs")).unwrap();
    assert!(
        index::check_update(&f.root(), &f.db())
            .unwrap()
            .changed
            .iter()
            .any(|p| p == "Page.razor")
    );
    let graph = f.index();
    for page in ["Page.razor", "View.cshtml"] {
        assert!(!file_relation(
            &graph,
            page,
            "uses_type",
            ("Nested/Service.cs", "Service")
        ));
    }
    assert!(!graph.nodes.iter().any(|n| n.file == "Service.cs"));
}
