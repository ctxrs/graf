use graf::{
    languages::{parse, supports},
    model::{Coverage, FileFacts, Node, QueryOptions, Reference},
    store::Store,
};

fn facts(path: &str, source: &str) -> FileFacts {
    let f = parse(path, source, "fixture").unwrap().unwrap();
    assert!(f.diagnostics.is_empty(), "{path}: {:?}", f.diagnostics);
    f
}
fn node<'a>(f: &'a FileFacts, label: &str) -> &'a Node {
    f.nodes
        .iter()
        .find(|n| n.label == label && n.id != f.nodes[0].id)
        .unwrap_or_else(|| panic!("missing {label}: {:?}", f.nodes))
}
fn refs<'a>(f: &'a FileFacts, relation: &str, label: &str) -> Vec<&'a Reference> {
    f.references
        .iter()
        .filter(|r| r.relation == relation && r.label == label)
        .collect()
}
fn key(f: &FileFacts, label: &str) -> String {
    node(f, label).binding_key.clone().unwrap()
}
fn stored(files: Vec<FileFacts>, source: &str, target: &str, relation: &str) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::create(&dir.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", files, vec![], Coverage::default())
        .unwrap();
    let graph = store.snapshot().unwrap();
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.source == source && e.target == target && e.relation == relation),
        "missing {relation}: {source} -> {target}; {:?}",
        graph.edges
    );
}
#[test]
fn scripted_dispatch_ranges_errors_and_identity() {
    for (path, source, label) in [
        ("sample.rb", "# π\ndef run\n  puts 'ok'\nend\n", "run"),
        (
            "sample.php",
            "<?php // π\nfunction run() { return 1; }",
            "run",
        ),
        (
            "sample.lua",
            "-- π\nlocal function run() return 1 end",
            "run",
        ),
        (
            "sample.luau",
            "-- π\nlocal function run(x: number): number return x end",
            "run",
        ),
        ("sample.sh", "# π\nrun() { echo ok; }", "run"),
        (
            "sample.ps1",
            "# π\nfunction Run { Write-Output 'ok' }",
            "Run",
        ),
        (
            "sample.ex",
            "# π\ndefmodule Sample do\n def run(), do: :ok\nend",
            "run",
        ),
    ] {
        assert!(supports(path));
        let f = facts(path, source);
        let n = node(&f, label);
        assert!(n.line.unwrap() >= 2);
        let start = n.metadata["start_byte"].as_u64().unwrap() as usize;
        let end = n.metadata["end_byte"].as_u64().unwrap() as usize;
        assert!(source.is_char_boundary(start) && source.is_char_boundary(end));
        assert!(source[start..end].contains(label));
        let same = facts(path, source);
        assert_eq!(n.id, node(&same, label).id);
        let other = facts(&format!("other/{path}"), source);
        assert_ne!(n.id, node(&other, label).id);
        assert!(
            f.edges
                .iter()
                .any(|e| e.target == n.id && e.relation == "contains")
        );
    }
    for (path, source) in [
        ("a.rb", "class {"),
        ("a.php", "<?php function {"),
        ("a.lua", "function ("),
        ("a.luau", "type ="),
        ("a.sh", "f() {"),
        ("a.ps1", "function F {"),
        ("a.ex", "defmodule Foo do"),
    ] {
        let f = parse(path, source, "bad").unwrap().unwrap();
        assert!(!f.diagnostics.is_empty(), "{path}");
        assert!(f.nodes.is_empty(), "{path}");
    }
    assert!(parse("../bad.rb", "", "h").is_err());
}
#[test]
fn ruby_classes_methods_inheritance_mixins_and_store() {
    let lib = facts(
        "lib.rb",
        "module Mix\nend\nclass Base\nend\nclass Worker < Base\n include ::Mix\n def self.run\n end\n def work\n  helper()\n end\n def helper\n end\nend",
    );
    assert_eq!(node(&lib, "Worker").kind, "class");
    assert_eq!(node(&lib, "run").kind, "method");
    assert_eq!(
        refs(&lib, "inherits", "Base")[0].candidate_keys,
        [key(&lib, "Base")]
    );
    assert_eq!(
        refs(&lib, "mixes_in", "::Mix")[0].source,
        node(&lib, "Worker").id
    );
    assert_eq!(
        refs(&lib, "calls", "helper")[0].candidate_keys,
        [key(&lib, "helper")]
    );
    let app = facts(
        "app.rb",
        "require_relative 'lib'\ndef entry\n ::Worker.run\nend",
    );
    let source = node(&app, "entry").id.clone();
    let target = node(&lib, "run").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
}
#[test]
fn ruby_typed_receivers_reassignment_and_anonymous_owners() {
    let f = facts(
        "typed.rb",
        "class Worker\n def work\n end\nend\nw = Worker.new\nw.work\nw = other\nw.work\n[1].each { |w| w.work }\nunknown.work\n",
    );
    let calls: Vec<_> = f
        .references
        .iter()
        .filter(|r| r.relation == "calls" && (r.label == "w.work" || r.label == "unknown.work"))
        .collect();
    assert_eq!(calls.len(), 4);
    assert!(calls.iter().all(|r| r.candidate_keys.is_empty()));
    assert_ne!(calls[2].source, f.nodes[0].id);
    assert_eq!(
        refs(&f, "instantiates", "Worker.new")[0].candidate_keys,
        [key(&f, "Worker")]
    );
    let f = facts(
        "typed.rb",
        "class Worker\n def work\n end\nend\nw = Worker.new\nw.work\n",
    );
    assert_eq!(
        refs(&f, "calls", "w.work")[0].candidate_keys,
        [key(&f, "work")]
    );
}
#[test]
fn ruby_nested_constants_factories_and_metaprogramming_barrier() {
    let f = facts(
        "types.rb",
        "module Outer\n module Inner\n  class Item\n  end\n end\nend\nPoint = Struct.new(:x) do\n def norm\n end\nend\nOuter::Inner::Item.new\n",
    );
    assert_eq!(key(&f, "Item"), "ruby:type:Outer::Inner::Item");
    assert_eq!(node(&f, "Point").kind, "class");
    assert!(
        node(&f, "norm")
            .qualified_name
            .as_ref()
            .unwrap()
            .contains("Point")
    );
    let unsafe_file = facts(
        "unsafe.rb",
        "class A\n def self.run; end\nend\nA.run\nA = other\n",
    );
    assert!(
        refs(&unsafe_file, "calls", "A.run")[0]
            .candidate_keys
            .is_empty()
    );
}
#[test]
fn php_namespaces_aliases_groups_inheritance_and_store() {
    let lib = facts(
        "types.php",
        "<?php namespace Domain; interface Contract {} trait Shared {} class Base {} class Worker extends Base implements Contract { use Shared; public static function run() {} }",
    );
    assert_eq!(
        refs(&lib, "inherits", "Base")[0].candidate_keys,
        [key(&lib, "Base")]
    );
    assert_eq!(
        refs(&lib, "implements", "Contract")[0].candidate_keys,
        [key(&lib, "Contract")]
    );
    assert_eq!(
        refs(&lib, "mixes_in", "Shared")[0].candidate_keys,
        [key(&lib, "Shared")]
    );
    let app = facts(
        "app.php",
        "<?php namespace App; use Domain\\{Worker as Task, Base}; function entry() { Task::run(); new Task(); }",
    );
    assert_eq!(
        refs(&app, "calls", "Task::run")[0].candidate_keys,
        [key(&lib, "run")]
    );
    assert_eq!(
        refs(&app, "instantiates", "Task")[0].candidate_keys,
        [key(&lib, "Worker")]
    );
    let source = node(&app, "entry").id.clone();
    let target = node(&lib, "run").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
}
#[test]
fn php_dynamic_calls_closures_and_namespace_collision() {
    let f = facts(
        "app.php",
        "<?php namespace A { function work() {} } namespace B { function work() {} function caller($work, $obj, $class) { $work(); $obj->work(); $class::work(); $f = fn() => work(); } }",
    );
    let works: Vec<_> = f.nodes.iter().filter(|n| n.label == "work").collect();
    assert_eq!(works.len(), 2);
    assert_ne!(works[0].binding_key, works[1].binding_key);
    for label in ["$work", "$obj->work", "$class::work"] {
        assert!(
            refs(&f, "calls", label)[0].candidate_keys.is_empty(),
            "{label}"
        );
    }
    let local = refs(&f, "calls", "work")[0];
    assert_eq!(local.candidate_keys, ["php:function:b\\work"]);
    assert_ne!(local.source, node(&f, "caller").id);
}
#[test]
fn lua_require_exports_methods_and_store() {
    let lib = facts(
        "tools.lua",
        "local M = {}\nfunction M.work() return 1 end\nfunction M:run() return 2 end\nreturn M\n",
    );
    let app = facts(
        "main.lua",
        "local tools = require('tools')\nlocal function main()\n tools.work()\nend\nmain()\n",
    );
    assert_eq!(
        refs(&app, "calls", "tools.work")[0].candidate_keys[0],
        key(&lib, "work")
    );
    assert_eq!(
        refs(&app, "imports", "tools")[0].candidate_keys[0],
        "lua:module:tools"
    );
    let source = node(&app, "main").id.clone();
    let target = node(&lib, "work").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
}
#[test]
fn lua_shadowing_callbacks_and_dynamic_require() {
    let f = facts(
        "main.lua",
        "local tools = require 'tools'\nlocal function run(tools) tools.work() end\ndo local tools = other; tools.work() end\ntools.work()\nhandler(function() require('inner') end)\nobj:require('wrong')\nrequire(variable)\n",
    );
    let calls = refs(&f, "calls", "tools.work");
    assert_eq!(calls.len(), 3);
    assert!(calls[0].candidate_keys.is_empty());
    assert!(calls[1].candidate_keys.is_empty());
    assert!(!calls[2].candidate_keys.is_empty());
    assert_ne!(refs(&f, "imports", "inner")[0].source, f.nodes[0].id);
    assert!(refs(&f, "imports", "wrong").is_empty());
    assert!(!refs(&f, "calls", "require").is_empty());
    let shadow = facts(
        "shadow.lua",
        "local function require(x) return x end\nrequire('wrong')",
    );
    assert!(shadow.references.iter().all(|r| r.relation != "imports"));
}
#[test]
fn luau_uses_distinct_typed_grammar_and_resolves_store() {
    let lib = facts(
        "math.luau",
        "export type Value = { value: number }\nlocal M = {}\nfunction M.twice(x: number): number return x * 2 end\nreturn M",
    );
    assert_eq!(node(&lib, "Value").kind, "type");
    assert_eq!(node(&lib, "twice").metadata["language"], "luau");
    let app = facts(
        "main.luau",
        "local math = require('math')\nlocal function main(): number return math.twice(2) end",
    );
    let source = node(&app, "main").id.clone();
    let target = node(&lib, "twice").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
}
#[test]
fn bash_source_functions_dynamic_commands_and_store() {
    let lib = facts("lib.sh", "work() { printf '%s' ok; }\n");
    let app = facts(
        "app.sh",
        "source ./lib.sh\nmain() { work; \"$command\"; $(build); }\nmain\n",
    );
    assert_eq!(
        refs(&app, "calls", "work")[0].candidate_keys,
        [key(&lib, "work")]
    );
    assert_eq!(
        refs(&app, "imports", "./lib.sh")[0].candidate_keys,
        ["bash:module:lib"]
    );
    assert!(app.references.iter().any(|r| r.relation == "calls"
        && r.label.contains("$command")
        && r.candidate_keys.is_empty()));
    let source = node(&app, "main").id.clone();
    let target = node(&lib, "work").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
}
#[test]
fn bash_nested_function_ownership_and_eval_barrier() {
    let f = facts(
        "app.sh",
        "work() { :; }\nouter() { work() { echo inner; }; work; }\nwork\n",
    );
    let works: Vec<_> = f.nodes.iter().filter(|n| n.label == "work").collect();
    assert_eq!(works.len(), 2);
    assert_ne!(works[0].binding_key, works[1].binding_key);
    assert_eq!(refs(&f, "calls", "work")[0].source, node(&f, "outer").id);
    let f = facts("dynamic.sh", "work() { :; }\neval \"$input\"\nwork");
    assert!(refs(&f, "calls", "work")[0].candidate_keys.is_empty());
}
#[test]
fn powershell_functions_classes_manifest_and_store() {
    let lib = facts(
        "tools.psm1",
        "function Get-Work { Write-Output 'ok' }\nclass Worker { [void] Run() { Get-Work } }\nenum State { Ready; Done }",
    );
    assert_eq!(node(&lib, "Worker").kind, "class");
    assert_eq!(node(&lib, "Run").kind, "method");
    assert_eq!(node(&lib, "State").kind, "enum");
    let app = facts(
        "app.ps1",
        "Import-Module './tools.psm1'\nfunction Main { get-work; & $command }\nMain",
    );
    assert_eq!(
        refs(&app, "calls", "get-work")[0].candidate_keys,
        [key(&lib, "Get-Work")]
    );
    assert!(
        app.references
            .iter()
            .any(|r| r.label == "$command" && r.candidate_keys.is_empty())
    );
    let source = node(&app, "Main").id.clone();
    let target = node(&lib, "Get-Work").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
    let manifest = facts(
        "tools.psd1",
        "@{ RootModule = 'tools.psm1'; RequiredModules = @('Other', @{ ModuleName = 'Pester'; ModuleVersion = '5.0' }) }",
    );
    let labels: Vec<_> = manifest
        .references
        .iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.label.as_str())
        .collect();
    assert!(
        labels.contains(&"tools.psm1") && labels.contains(&"Other") && labels.contains(&"Pester")
    );
    assert!(!labels.contains(&"5.0"));
}
#[test]
fn powershell_case_insensitive_local_scopes_and_alias_barriers() {
    let f = facts(
        "app.ps1",
        "function Get-Work { }\nfunction Main { function Get-Work {}\nGET-WORK\n& { Get-Work } }\nget-work",
    );
    let locals: Vec<_> = f.nodes.iter().filter(|n| n.label == "Get-Work").collect();
    assert_eq!(locals.len(), 2);
    assert_ne!(locals[0].binding_key, locals[1].binding_key);
    assert_eq!(
        refs(&f, "calls", "GET-WORK")[0].candidate_keys,
        [locals[1].binding_key.clone().unwrap()]
    );
    let f = facts(
        "aliases.ps1",
        "function Get-Work {}\nSet-Alias Get-Work Other\nGet-Work",
    );
    assert!(refs(&f, "calls", "Get-Work")[0].candidate_keys.is_empty());
}
#[test]
fn elixir_alias_import_arity_nested_modules_and_store() {
    let lib = facts(
        "worker.ex",
        "defmodule Work.Tools do\n def run(x), do: x\n defp hidden(), do: :ok\nend",
    );
    let app = facts(
        "main.ex",
        "defmodule Main do\n alias Work.Tools, as: T\n import Work.Tools, only: [run: 1]\n def main() do\n  T.run(1)\n  run(2)\n  run()\n  hidden()\n end\n defmodule Nested do\n  alias Other.Tools, as: T\n  def nested(), do: T.run(3)\n end\n def after_nested(), do: T.run(4)\nend",
    );
    assert_eq!(
        refs(&app, "calls", "T.run")[0].candidate_keys,
        [key(&lib, "run")]
    );
    assert_eq!(
        refs(&app, "calls", "run")[0].candidate_keys,
        [key(&lib, "run")]
    );
    assert!(refs(&app, "calls", "run")[1].candidate_keys.is_empty());
    assert!(refs(&app, "calls", "hidden")[0].candidate_keys.is_empty());
    assert_eq!(
        refs(&app, "calls", "T.run")[1].candidate_keys,
        ["elixir:function:Other.Tools:run/1"]
    );
    assert_eq!(
        refs(&app, "calls", "T.run")[2].candidate_keys,
        [key(&lib, "run")]
    );
    assert_eq!(key(&app, "Nested"), "elixir:module-name:Main.Nested");
    assert!(key(&lib, "hidden").starts_with("elixir:private:"));
    let source = node(&app, "main").id.clone();
    let target = node(&lib, "run").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
}
#[test]
fn elixir_clauses_guards_pipelines_defaults_and_dynamic_function_calls() {
    let f = facts(
        "worker.ex",
        "defmodule Work do\n def run(x) when is_integer(x), do: x\n def run(x), do: x\n def plus(x, y \\\\ 1), do: x + y\n def main(f) do\n  f.(1)\n  1 |> run()\n  plus(2)\n  fn x -> run(x) end\n end\nend",
    );
    assert_eq!(f.nodes.iter().filter(|n| n.label == "run").count(), 1);
    assert_eq!(refs(&f, "calls", "run")[0].candidate_keys, [key(&f, "run")]);
    assert!(refs(&f, "calls", "f.")[0].candidate_keys.is_empty());
    assert!(
        node(&f, "plus").metadata["binding_aliases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "elixir:function:Work:plus/1")
    );
    let main = node(&f, "main").id.clone();
    let plus = node(&f, "plus").id.clone();
    stored(vec![f], &main, &plus, "calls");
}
#[test]
fn elixir_conflicting_imports_and_external_modules_stay_unresolved() {
    let filtered = facts(
        "filtered.ex",
        "defmodule Filtered do\n import A, only: [run: 1]\n import B, except: [run: 1]\n def main() do\n  run(1)\n  run()\n end\nend",
    );
    assert_eq!(
        refs(&filtered, "calls", "run")[0].candidate_keys,
        ["elixir:function:A:run/1"]
    );
    assert_eq!(
        refs(&filtered, "calls", "run")[1].candidate_keys,
        ["elixir:function:B:run/0"]
    );
    let f = facts(
        "app.ex",
        "defmodule App do\n import A\n import B\n def main() do\n  run()\n  object.run()\n end\nend",
    );
    assert!(refs(&f, "calls", "run")[0].candidate_keys.is_empty());
    assert!(refs(&f, "calls", "object.run")[0].candidate_keys.is_empty());
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::create(&dir.path().join("g.db")).unwrap();
    store
        .apply_native("fixture", vec![f], vec![], Coverage::default())
        .unwrap();
    assert!(store.stats().unwrap().unresolved_references >= 4);
    assert!(
        !store
            .query("main", &QueryOptions::default())
            .unwrap()
            .nodes
            .is_empty()
    );
}

#[test]
fn ruby_module_function_exports_and_method_local_environment() {
    let lib = facts(
        "tools.rb",
        "module Tools\n module_function\n def work; end\nend",
    );
    let app = facts("app.rb", "Tools.work");
    let source = app.nodes[0].id.clone();
    let target = node(&lib, "work").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
    let f = facts(
        "locals.rb",
        "class Worker\n def work; end\nend\nw = Worker.new\ndef main\n w.work\nend",
    );
    assert!(refs(&f, "calls", "w.work")[0].candidate_keys.is_empty());
    let f = facts(
        "extended.rb",
        "module Tools\n extend self\n def work; end\nend\nTools.work",
    );
    let source = f.nodes[0].id.clone();
    let target = node(&f, "work").id.clone();
    stored(vec![f], &source, &target, "calls");
}
#[test]
fn lua_declaration_order_and_mutated_exports_do_not_guess() {
    let f = facts("order.lua", "work()\nlocal function work() end\nwork()\n");
    let c = refs(&f, "calls", "work");
    assert!(c[0].candidate_keys.is_empty());
    assert_eq!(c[1].candidate_keys, [key(&f, "work")]);
    let f = facts(
        "mutated.lua",
        "local M = {}\nfunction M.work() end\nM.work = other\nreturn M",
    );
    assert!(node(&f, "work").binding_key.is_none());
    let lib = facts("private.lua", "local function hidden() end\nreturn {}");
    let app = facts("app.lua", "local lib = require('private')\nlib.hidden()");
    assert_ne!(
        refs(&app, "calls", "lib.hidden")[0].candidate_keys[0],
        key(&lib, "hidden")
    );
}
#[test]
fn powershell_static_methods_and_imported_bases_have_qualified_keys() {
    let lib = facts("worker.psm1", "class Worker { static [void] Run() {} }");
    let app = facts(
        "app.ps1",
        "using module './worker.psm1'\nclass Child : Worker {}\n[Worker]::Run()",
    );
    assert_eq!(
        refs(&app, "calls", "[Worker].Run")[0].candidate_keys,
        [key(&lib, "Run")]
    );
    assert_eq!(
        refs(&app, "inherits", "Worker")[0].candidate_keys,
        [key(&lib, "Worker")]
    );
    let source = app.nodes[0].id.clone();
    let target = node(&lib, "Run").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
}
#[test]
fn elixir_protocol_implementations_and_use_keep_their_module_identity() {
    let f = facts(
        "protocol.ex",
        "defprotocol Render do\n def render(value)\nend\ndefimpl Render, for: String do\n def render(value), do: value\nend\ndefmodule User do\n use Render\nend",
    );
    let protocol = f.nodes.iter().find(|n| n.kind == "interface").unwrap();
    let implementation = f.nodes.iter().find(|n| n.kind == "impl").unwrap();
    assert_ne!(protocol.binding_key, implementation.binding_key);
    assert_eq!(
        refs(&f, "implements", "Render")[0].candidate_keys,
        [protocol.binding_key.clone().unwrap()]
    );
    assert_eq!(refs(&f, "imports", "Render")[0].source, node(&f, "User").id);
}

#[test]
fn luau_table_metatable_pattern_preserves_other_exported_methods() {
    let lib = facts(
        "server.luau",
        "local Server = {}\nServer.__index = Server\nfunction Server.new(value: number): number return value end\nfunction Server:run(): () end\nreturn Server",
    );
    assert_eq!(key(&lib, "new"), "luau:server:new");
    assert_eq!(key(&lib, "run"), "luau:server:run");
    let app = facts(
        "app.luau",
        "local Server = require('server')\nlocal function launch(Server: any) Server.new(2) end\nServer.new(1)",
    );
    let calls = refs(&app, "calls", "Server.new");
    assert!(calls[0].candidate_keys.is_empty());
    assert_eq!(calls[1].candidate_keys[0], key(&lib, "new"));
    let source = app.nodes[0].id.clone();
    let target = node(&lib, "new").id.clone();
    stored(vec![lib, app], &source, &target, "calls");
}

#[test]
fn shebang_shared_tokenizer_and_named_parser_preserve_actual_paths() {
    use graf::languages::scripted::{parse_named, shebang_language};
    for (interpreter, language) in [
        ("sh", "bash"),
        ("bash", "bash"),
        ("ruby", "ruby"),
        ("php", "php"),
        ("lua", "lua"),
        ("luau", "luau"),
        ("pwsh", "powershell"),
        ("elixir", "elixir"),
        ("node", "javascript"),
        ("nodejs", "javascript"),
        ("julia", "julia"),
        ("python", "python"),
        ("python2", "python"),
        ("python3", "python"),
    ] {
        assert_eq!(
            shebang_language(&format!("#!/usr/bin/env -S {interpreter} -x\n")),
            Some(language)
        );
        assert_eq!(
            shebang_language(&format!("#!/usr/bin/{interpreter}\n")),
            Some(language)
        );
    }
    assert_eq!(shebang_language("#!/usr/bin/env -u ruby\n"), None);
    assert_eq!(shebang_language("# #!/usr/bin/bash\n"), None);
    let f = parse_named(
        "bin/task",
        "#!/usr/bin/env bash\nrun() { :; }",
        "hash",
        "bash",
    )
    .unwrap()
    .unwrap();
    assert_eq!(f.path, "bin/task");
    assert_eq!(f.hash, "hash");
    assert_eq!(node(&f, "run").file, "bin/task");
    assert!(
        parse_named("bin/task", "", "h", "python")
            .unwrap()
            .is_none()
    );
    assert!(parse_named("../task", "", "h", "bash").is_err());
}
#[test]
fn lua_toc_manifest_and_cross_dialect_require_resolve_without_loading_files() {
    let lua = facts(
        "Addon/core.lua",
        "local M = {}\nfunction M.run() end\nreturn M",
    );
    let luau = facts(
        "Addon/typed.luau",
        "local M = {}\nfunction M.run(x: number): number return x end\nreturn M",
    );
    let manifest = facts(
        "Addon/Addon.toc",
        "## Interface: 110000\n## Title: Δ Addon\n## Dependencies: External\n# comment\ncore.lua\ntyped.luau\nUi.xml\n",
    );
    assert_eq!(manifest.nodes[0].kind, "manifest");
    assert_eq!(manifest.nodes[0].metadata["manifest"]["Title"], "Δ Addon");
    assert!(
        refs(&manifest, "imports", "External")[0]
            .candidate_keys
            .is_empty()
    );
    let source = manifest.nodes[0].id.clone();
    let target = lua.nodes[0].id.clone();
    stored(
        vec![lua.clone(), luau.clone(), manifest],
        &source,
        &target,
        "imports",
    );
    for (path, source, lib) in [
        (
            "lua-app.lua",
            "local m = require('Addon.typed')\nm.run(1)",
            luau,
        ),
        (
            "luau-app.luau",
            "local m = require('Addon.core')\nm.run()",
            lua,
        ),
    ] {
        let app = facts(path, source);
        let source = app.nodes[0].id.clone();
        let target = node(&lib, "run").id.clone();
        stored(vec![lib, app], &source, &target, "calls");
    }
    let unsafe_manifest = parse("Bad.toc", "../../outside.lua\n", "h")
        .unwrap()
        .unwrap();
    assert!(!unsafe_manifest.diagnostics.is_empty());
    assert!(unsafe_manifest.references[0].candidate_keys.is_empty());
}
#[test]
fn php_static_framework_patterns_bind_exact_namespaced_types() {
    let f = facts(
        "framework.php",
        r#"<?php
namespace App;
class Provider {
 protected $listen = [Event::class => [Listener::class]];
 public function register(): void {
  $this->app->bind(Contract::class, Implementation::class);
  $this->app->singleton(Contract::class, Implementation::class);
  config('limits.requests', 10);
 }
}
interface Contract {} class Implementation {} class Event {} class Listener {} class Limits {}
"#,
    );
    assert_eq!(
        refs(&f, "bound_to", "app\\implementation")[0].source,
        node(&f, "Contract").id
    );
    assert_eq!(
        refs(&f, "listened_by", "app\\listener")[0].source,
        node(&f, "Event").id
    );
    assert_eq!(
        refs(&f, "uses_config", "limits.requests")[0].source,
        node(&f, "register").id
    );
    let contract = node(&f, "Contract").id.clone();
    let implementation = node(&f, "Implementation").id.clone();
    stored(vec![f], &contract, &implementation, "bound_to");
}
#[test]
fn php_framework_helpers_do_not_capture_shadowed_or_dynamic_calls() {
    let f = facts(
        "shadow.php",
        r#"<?php
namespace App;
class Limits {} class Contract {} class Implementation {}
function use_helpers($obj, $key) {
 config('limits.rate'); config($key);
 $obj->bind(Contract::class, Implementation::class);
 $this->other->bind(Contract::class, Implementation::class);
 $this->app->bind(Contract::VALUE, Implementation::class);
}
function config($key) { return null; }
"#,
    );
    assert!(
        refs(&f, "uses_config", "limits.rate")[0]
            .candidate_keys
            .is_empty()
    );
    assert!(f.references.iter().all(|r| r.relation != "bound_to"));
    let imported = facts(
        "alias.php",
        "<?php use function Vendor\\lookup as config; class Limits {} config('limits.rate');",
    );
    assert!(
        imported
            .references
            .iter()
            .all(|r| r.relation != "uses_config")
    );
}
#[test]
fn php_properties_constants_and_signature_types_resolve_through_aliases() {
    let lib = facts(
        "types.php",
        "<?php namespace Types; class Item { public static string $color = 'blue'; public const CODE = 1; }",
    );
    let app = facts(
        "app.php",
        "<?php namespace App; use Types\\Item as Value; class User { public Value $item; public function run(Value $input): Value { Value::$color; Value::CODE; return $input; } }",
    );
    assert_eq!(node(&app, "$item").kind, "property");
    assert_eq!(
        refs(&app, "uses_static_prop", "Value::$color")[0].candidate_keys,
        [key(&lib, "Item")]
    );
    assert_eq!(
        refs(&app, "references_constant", "Value::CODE")[0].candidate_keys,
        [key(&lib, "Item")]
    );
    let contexts = node(&app, "run").metadata["type_contexts"]
        .as_array()
        .unwrap();
    assert!(contexts.iter().any(|c| c["context"] == "parameter_type"));
    assert!(contexts.iter().any(|c| c["context"] == "return_type"));
    let source = node(&app, "$item").id.clone();
    let target = node(&lib, "Item").id.clone();
    stored(vec![lib, app], &source, &target, "references");
}
#[test]
fn powershell_properties_type_contexts_and_manifest_exports_are_static() {
    let module = facts(
        "tools.psm1",
        "enum State { Ready; Done }\nfunction Get-Work {}\nclass Worker { [State] $Status; [State] Run([State] $input) { return $input } }",
    );
    assert_eq!(node(&module, "$Status").kind, "property");
    let contexts = node(&module, "Run").metadata["type_contexts"]
        .as_array()
        .unwrap();
    assert!(contexts.iter().any(|c| c["context"] == "parameter_type"));
    assert!(contexts.iter().any(|c| c["context"] == "return_type"));
    let manifest = facts(
        "tools.psd1",
        "@{ 'RootModule' = 'tools.psm1'; NestedModules = @('nested.psm1'); RequiredModules = @(@{ 'ModuleName' = 'External'; 'ModuleVersion' = '1.0' }); FunctionsToExport = @('Get-Work', '*'); CmdletsToExport = @('Invoke-Native') }",
    );
    assert_eq!(manifest.nodes[0].kind, "manifest");
    assert!(refs(&manifest, "imports", "ModuleName").is_empty());
    assert!(refs(&manifest, "imports", "1.0").is_empty());
    assert!(refs(&manifest, "exports", "*")[0].candidate_keys.is_empty());
    let source = manifest.nodes[0].id.clone();
    let target = node(&module, "Get-Work").id.clone();
    stored(vec![module, manifest], &source, &target, "exports");
}
#[test]
fn bash_only_proven_current_file_path_idioms_resolve() {
    let lib = facts("lib/work.sh", "work() { :; }");
    for source in [
        "source \"$(dirname \"${BASH_SOURCE[0]}\")/../lib/work.sh\"\nwork\n",
        "ROOT=\"$(cd \"$(dirname \"${BASH_SOURCE[0]}\")/..\" && pwd)\"\nsource \"${ROOT}/lib/work.sh\"\nwork\n",
        "DIR=\"$(dirname \"${BASH_SOURCE[0]}\")\"\nsource \"${DIR}/../lib/work.sh\"\nwork\n",
    ] {
        let app = facts("scripts/run.sh", source);
        let source = app.nodes[0].id.clone();
        let target = node(&lib, "work").id.clone();
        assert_eq!(
            refs(&app, "calls", "work")[0].candidate_keys,
            [key(&lib, "work")]
        );
        stored(vec![lib.clone(), app], &source, &target, "calls");
    }
    for source in [
        "source \"${EXTERNAL}/lib/work.sh\"",
        "source \"$(unknown)/lib/work.sh\"",
        "dirname() { echo wrong; }\nsource \"$(dirname \"${BASH_SOURCE[0]}\")/../lib/work.sh\"",
        "cd() { :; }\nROOT=\"$(cd \"$(dirname \"${BASH_SOURCE[0]}\")/..\" && pwd)\"\nsource \"${ROOT}/lib/work.sh\"",
        "pwd() { echo wrong; }\nROOT=\"$(cd \"$(dirname \"${BASH_SOURCE[0]}\")/..\" && pwd)\"\nsource \"${ROOT}/lib/work.sh\"",
        "DIR=\"$(dirname \"${BASH_SOURCE[0]}\")\"\nDIR=$EXTERNAL\nsource \"${DIR}/lib/work.sh\"",
        "source \"$(dirname \"${BASH_SOURCE[0]}\")/../../../escape.sh\"",
    ] {
        let app = facts("scripts/run.sh", &format!("{source}\nwork\n"));
        assert!(
            app.references
                .iter()
                .filter(|r| r.relation == "imports")
                .all(|r| r.candidate_keys.is_empty()),
            "{source}"
        );
        assert!(refs(&app, "calls", "work")[0].candidate_keys.is_empty());
    }
    let shadow = facts("shadow.sh", "source() { :; }\nsource './lib/work.sh'");
    assert!(shadow.references.iter().all(|r| r.relation != "imports"));
}

#[test]
fn ruby_literal_attributes_have_owned_ranges_and_cross_file_calls() {
    let source = "class Account\n attr_reader :name\n attr_writer 'secret'\n attr_accessor :balance, :\"café\"\nend\n";
    let lib = facts("account.rb", source);
    for (name, generator) in [
        ("name", "attr_reader"),
        ("secret=", "attr_writer"),
        ("balance", "attr_accessor"),
        ("balance=", "attr_accessor"),
        ("café", "attr_accessor"),
        ("café=", "attr_accessor"),
    ] {
        let method = node(&lib, name);
        assert_eq!(method.kind, "method");
        assert_eq!(method.metadata["ruby_generated_by"], generator);
        assert_eq!(
            method.binding_key.as_deref(),
            Some(format!("ruby:instance:Account#{name}").as_str())
        );
        let start = method.metadata["start_byte"].as_u64().unwrap() as usize;
        let end = method.metadata["end_byte"].as_u64().unwrap() as usize;
        assert!(source.is_char_boundary(start) && source.is_char_boundary(end));
        assert!(source[start..end].contains(name.trim_end_matches('=')));
        assert!(
            lib.edges
                .iter()
                .any(|e| e.source == node(&lib, "Account").id
                    && e.target == method.id
                    && e.relation == "contains")
        );
    }
    assert!(
        !lib.nodes
            .iter()
            .any(|n| matches!(n.label.as_str(), "name=" | "secret"))
    );
    let app = facts(
        "app.rb",
        "def main\n a = Account.new\n a.name\n a.secret = 'x'\n a.balance\n a.balance = 3\n a.café\nend\n",
    );
    assert_eq!(
        refs(&app, "calls", "a.secret=")[0].candidate_keys,
        [key(&lib, "secret=")]
    );
    let caller = node(&app, "main").id.clone();
    for name in ["name", "secret=", "balance", "balance=", "café"] {
        stored(
            vec![lib.clone(), app.clone()],
            &caller,
            &node(&lib, name).id,
            "calls",
        );
    }
}

#[test]
fn ruby_literal_inheritance_resolves_exact_ancestors_and_method_kinds() {
    let grand = facts(
        "grand.rb",
        "class Grand\n attr_reader :title\n def work; :ok; end\n def self.publish; :ok; end\nend\n",
    );
    let base = facts("parent.rb", "class Parent < Grand\nend\n");
    let child = facts(
        "child.rb",
        "class Child < Parent\n def relay\n  work\n end\n def self.relay_class\n  publish()\n end\nend\n",
    );
    let other = facts("other.rb", "class Other\n def work; :other; end\nend\n");
    let app = facts(
        "app.rb",
        "def main\n worker = Child.new\n worker.work\n worker.title\nend\nChild.publish\n",
    );
    let all = vec![
        grand.clone(),
        base.clone(),
        child.clone(),
        other,
        app.clone(),
    ];
    for (source, target, relation) in [
        (
            node(&child, "Child").id.clone(),
            node(&base, "Parent").id.clone(),
            "inherits",
        ),
        (
            node(&app, "main").id.clone(),
            node(&grand, "work").id.clone(),
            "calls",
        ),
        (
            node(&app, "main").id.clone(),
            node(&grand, "title").id.clone(),
            "calls",
        ),
        (
            node(&child, "relay").id.clone(),
            node(&grand, "work").id.clone(),
            "calls",
        ),
        (
            node(&child, "relay_class").id.clone(),
            node(&grand, "publish").id.clone(),
            "calls",
        ),
        (
            app.nodes[0].id.clone(),
            node(&grand, "publish").id.clone(),
            "calls",
        ),
    ] {
        stored(all.clone(), &source, &target, relation);
    }
}

#[test]
fn ruby_inherited_overrides_super_and_unknown_receivers_are_distinct() {
    let base = facts(
        "base.rb",
        "class Base\n def work; :base; end\n attr_reader :name\nend\n",
    );
    let child = facts(
        "child.rb",
        "class Child < Base\n attr_reader :work\n def name; :child; end\nend\nclass SuperChild < Base\n def work\n  super\n end\nend\n",
    );
    let app = facts(
        "app.rb",
        "def main\n worker = Child.new\n worker.work\n worker.name\nend\ndef unknown(worker)\n worker.work\nend\ndef reassigned\n worker = Child.new\n worker = unknown\n worker.work\nend\n",
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&tmp.path().join("ruby.db")).unwrap();
    store
        .apply_native(
            "fixture",
            vec![base.clone(), child.clone(), app.clone()],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let graph = store.snapshot().unwrap();
    let main = &node(&app, "main").id;
    for name in ["work", "name"] {
        assert!(graph.edges.iter().any(|e| &e.source == main
            && e.target == node(&child, name).id
            && e.relation == "calls"));
        assert!(!graph.edges.iter().any(|e| &e.source == main
            && e.target == node(&base, name).id
            && e.relation == "calls"));
    }
    for name in ["unknown", "reassigned"] {
        assert!(!graph.edges.iter().any(|e| e.source == node(&app, name).id
            && e.relation == "calls"
            && e.target == node(&base, "work").id));
        assert!(
            refs(&app, "calls", "worker.work")
                .iter()
                .filter(|r| r.source == node(&app, name).id)
                .all(|r| r.candidate_keys.is_empty())
        );
    }
    let super_method = child
        .nodes
        .iter()
        .find(|n| n.binding_key.as_deref() == Some("ruby:instance:SuperChild#work"))
        .unwrap();
    assert!(graph.edges.iter().any(|e| e.source == super_method.id
        && e.target == node(&base, "work").id
        && e.relation == "calls"));
}

#[test]
fn ruby_generated_methods_respect_literal_order_and_macro_shadows() {
    let lib = facts(
        "ordered.rb",
        "class Ordered\n attr_reader :value\n def value; :explicit; end\n def other; :old; end\n attr_reader :other\nend\n",
    );
    for (key, generated) in [
        ("ruby:instance:Ordered#value", false),
        ("ruby:instance:Ordered#other", true),
    ] {
        let methods: Vec<_> = lib
            .nodes
            .iter()
            .filter(|n| n.binding_key.as_deref() == Some(key))
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(
            methods[0].metadata["ruby_generated_by"].is_string(),
            generated
        );
        let app = facts(
            "app.rb",
            &format!(
                "def main\n item = Ordered.new\n item.{}\nend",
                methods[0].label
            ),
        );
        stored(
            vec![lib.clone(), app.clone()],
            &node(&app, "main").id,
            &methods[0].id,
            "calls",
        );
    }
    for source in [
        "class Item\n def self.attr_reader(name); end\n attr_reader :value\nend",
        "class Item\n attr_reader dynamic_name\nend",
        "class Item\n attr_reader :\"value_#{suffix}\"\nend",
        "class Item\n attr_reader :value if enabled\nend",
        "class Item\n def configure\n  attr_accessor :value\n end\nend",
        "class Item\n Other.attr_reader :value\nend",
    ] {
        let f = facts("dynamic.rb", source);
        assert!(
            !f.nodes
                .iter()
                .any(|n| n.metadata["ruby_generated_by"].is_string()),
            "{source}"
        );
    }
}

#[test]
fn ruby_inherited_lookup_rejects_ambiguous_dynamic_and_shadowed_ancestry() {
    ruby_inheritance_shadow_and_depth_assertions();
    let base = facts(
        "base.rb",
        "class Base\n def work; :base; end\n def self.publish; :base; end\nend\n",
    );
    let app = facts(
        "app.rb",
        "def main\n worker = Child.new\n worker.work\nend\nChild.publish\n",
    );
    for (child_source, extra_source) in [
        ("class Child < choose_base(); end", ""),
        ("class Child < Base; end", "class Child; end"),
        (
            "class Child < Base; end",
            "class Base\n def work; :other; end\nend",
        ),
        (
            "class Child < Base\n def work; :one; end\n def work; :two; end\nend",
            "",
        ),
        (
            "class Child < Base\n include External\n extend External\nend",
            "",
        ),
        (
            "class Child < Base\n self.prepend External\n extend External\nend",
            "",
        ),
        (
            "class Child < Base; end",
            "module R\n refine Base do\n  def work; :refined; end\n end\nend",
        ),
        (
            "class Child < Base; end",
            "AliasChild = Child\nAliasChild.class_eval { attr_reader :work }",
        ),
        ("class Child < Base; end", "def Child.work; :other; end"),
        ("class Child < Base; end", "Child.include External"),
        (
            "class Child < Base; end",
            "Base, Spare = resolve_constants()",
        ),
        ("class Child < Cycle; end\nclass Cycle < Child; end", ""),
    ] {
        let child = facts("child.rb", child_source);
        let extra = facts("extra.rb", extra_source);
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::create(&tmp.path().join("ruby.db")).unwrap();
        store
            .apply_native(
                "fixture",
                vec![base.clone(), child, extra, app.clone()],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        let graph = store.snapshot().unwrap();
        assert!(
            !graph.edges.iter().any(|e| e.source == node(&app, "main").id
                && e.target == node(&base, "work").id
                && e.relation == "calls"),
            "{child_source}\n{extra_source}"
        );
    }
    let mismatch = facts(
        "child.rb",
        "class Child < Base\n def self.call\n  work()\n end\nend",
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&tmp.path().join("ruby.db")).unwrap();
    store
        .apply_native(
            "fixture",
            vec![base, mismatch.clone()],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    assert!(
        !store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.source == node(&mismatch, "call").id && e.relation == "calls")
    );
}

#[test]
fn ruby_store_rebinds_inheritance_after_overrides_barriers_and_deletions() {
    let base = facts("base.rb", "class Base\n def work; :base; end\nend");
    let child = facts("child.rb", "class Child < Base; end");
    let app = facts("app.rb", "def main\n worker = Child.new\n worker.work\nend");
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&tmp.path().join("ruby.db")).unwrap();
    let caller = node(&app, "main").id.clone();
    let has_call = |store: &Store, target: &str| {
        store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.source == caller && e.target == target && e.relation == "calls")
    };
    store
        .apply_native(
            "fixture",
            vec![base.clone(), child.clone(), app],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    assert!(has_call(&store, &node(&base, "work").id));
    let override_child = facts("child.rb", "class Child < Base\n attr_reader :work\nend");
    store
        .apply_native(
            "fixture",
            vec![override_child.clone()],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    assert!(has_call(&store, &node(&override_child, "work").id));
    assert!(!has_call(&store, &node(&base, "work").id));
    store
        .apply_native("fixture", vec![child], vec![], Coverage::default())
        .unwrap();
    assert!(has_call(&store, &node(&base, "work").id));
    let barrier = facts(
        "scripts/patch",
        "#!/usr/bin/env ruby\nBase.class_eval { attr_reader :work }\n",
    );
    store
        .apply_native("fixture", vec![barrier], vec![], Coverage::default())
        .unwrap();
    assert!(!has_call(&store, &node(&base, "work").id));
    store
        .apply_native(
            "fixture",
            vec![],
            vec!["scripts/patch".into()],
            Coverage::default(),
        )
        .unwrap();
    assert!(has_call(&store, &node(&base, "work").id));
    let mut old_schema = base.clone();
    old_schema.nodes[0]
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("ruby_lookup_schema");
    store
        .apply_native("fixture", vec![old_schema], vec![], Coverage::default())
        .unwrap();
    assert!(!has_call(&store, &node(&base, "work").id));
    store
        .apply_native("fixture", vec![base.clone()], vec![], Coverage::default())
        .unwrap();
    assert!(has_call(&store, &node(&base, "work").id));
    store
        .apply_native(
            "fixture",
            vec![],
            vec!["base.rb".into()],
            Coverage::default(),
        )
        .unwrap();
    assert!(!has_call(&store, &node(&base, "work").id));
}

fn ruby_inheritance_shadow_and_depth_assertions() {
    let base = facts("base.rb", "class Base\n def work; :global; end\nend");
    for (source, receiver) in [
        (
            "module N\n class Base; end\n class Child < Base; end\nend",
            "N::Child",
        ),
        (
            "class Provider\n class Base; end\nend\nclass Scope < Provider\n class Child < Base; end\nend",
            "Scope::Child",
        ),
        (
            "module N\n class Base\n  def work; :nested; end\n end\nend\nclass N::Child < MissingBase; end",
            "N::Child",
        ),
    ] {
        let child = facts("child.rb", source);
        let app = facts(
            "app.rb",
            &format!("def main\n item = {receiver}.new\n item.work\nend"),
        );
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::create(&tmp.path().join("ruby.db")).unwrap();
        store
            .apply_native(
                "fixture",
                vec![base.clone(), child.clone(), app.clone()],
                vec![],
                Coverage::default(),
            )
            .unwrap();
        assert!(
            !store
                .snapshot()
                .unwrap()
                .edges
                .iter()
                .any(|e| e.source == node(&app, "main").id && e.relation == "calls"),
            "{source}"
        );
        if receiver == "Scope::Child" {
            assert!(
                !store
                    .snapshot()
                    .unwrap()
                    .edges
                    .iter()
                    .any(|e| e.source == node(&child, "Child").id
                        && e.target == node(&base, "Base").id
                        && e.relation == "inherits")
            );
        }
    }
    let mut declarations = "class C0\n def work; :ok; end\nend\n".to_owned();
    for i in 1..=64 {
        declarations.push_str(&format!("class C{i} < C{}; end\n", i - 1));
    }
    let chain = facts("chain.rb", &declarations);
    let app = facts(
        "app.rb",
        "def near\n item = C63.new\n item.work\nend\ndef far\n item = C64.new\n item.work\nend",
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&tmp.path().join("ruby.db")).unwrap();
    store
        .apply_native(
            "fixture",
            vec![chain.clone(), app.clone()],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    let graph = store.snapshot().unwrap();
    assert!(graph.edges.iter().any(|e| e.source == node(&app, "near").id
        && e.target == node(&chain, "work").id
        && e.relation == "calls"));
    assert!(
        !graph
            .edges
            .iter()
            .any(|e| e.source == node(&app, "far").id && e.relation == "calls")
    );
}

#[test]
fn ruby_included_constants_block_superclass_fallback_and_rebind_on_change() {
    let base = facts(
        "base.rb",
        "class Base\n def work; :wrong; end\nend\nmodule Provider\n class Base\n  def work; :right; end\n end\nend",
    );
    let app = facts(
        "app.rb",
        "def main\n item = Scope::Child.new\n item.work\nend",
    );
    let caller = node(&app, "main").id.clone();
    let global = base
        .nodes
        .iter()
        .find(|n| n.binding_key.as_deref() == Some("ruby:instance:Base#work"))
        .unwrap()
        .id
        .clone();
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&tmp.path().join("ruby.db")).unwrap();
    store
        .apply_native("fixture", vec![base, app], vec![], Coverage::default())
        .unwrap();
    for mixin in ["", "include Provider", "", "prepend Provider"] {
        let child = facts(
            "child.rb",
            &format!("class Scope\n {mixin}\n class Child < Base\n end\nend"),
        );
        store
            .apply_native("fixture", vec![child.clone()], vec![], Coverage::default())
            .unwrap();
        let graph = store.snapshot().unwrap();
        let calls: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.source == caller && e.relation == "calls")
            .collect();
        let inherits: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.source == node(&child, "Child").id && e.relation == "inherits")
            .collect();
        if mixin.is_empty() {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].target, global);
            assert_eq!(inherits.len(), 1);
            assert!(graph.nodes.iter().any(|n| n.id == inherits[0].target
                && n.binding_key.as_deref() == Some("ruby:type:Base")));
        } else {
            assert!(calls.is_empty(), "{mixin}");
            assert!(inherits.is_empty(), "{mixin}");
        }
    }
    let local = facts(
        "child.rb",
        "class Scope\n include Provider\n class Base\n  def work; :local; end\n end\n class Child < Base; end\nend",
    );
    let target = node(&local, "work").id.clone();
    store
        .apply_native("fixture", vec![local], vec![], Coverage::default())
        .unwrap();
    assert!(
        store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.source == caller && e.target == target && e.relation == "calls")
    );
}

#[test]
fn ruby_private_inherited_methods_and_accessors_require_a_self_call() {
    let child = facts(
        "child.rb",
        "class Child < Base\n def internal\n  work\n  token\n  self.work\n  self.token = 1\n end\nend",
    );
    let app = facts(
        "app.rb",
        "def external\n worker = Child.new\n worker.work\n worker.token\n worker.token = 2\n worker.exposed\nend",
    );
    let internal = node(&child, "internal").id.clone();
    let external = node(&app, "external").id.clone();
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&tmp.path().join("ruby.db")).unwrap();
    store
        .apply_native("fixture", vec![child, app], vec![], Coverage::default())
        .unwrap();
    for visibility in ["public", "private", "protected", "public"] {
        let base = facts(
            "base.rb",
            &format!(
                "class Base\n {visibility}\n def work; :secret; end\n attr_accessor :token\n public\n def exposed; :ok; end\nend"
            ),
        );
        for name in ["work", "token", "token="] {
            assert_eq!(node(&base, name).metadata["ruby_visibility"], visibility);
        }
        store
            .apply_native("fixture", vec![base.clone()], vec![], Coverage::default())
            .unwrap();
        let graph = store.snapshot().unwrap();
        for name in ["work", "token", "token="] {
            let target = &node(&base, name).id;
            assert!(
                graph
                    .edges
                    .iter()
                    .any(|e| e.source == internal && &e.target == target && e.relation == "calls"),
                "{visibility} {name}"
            );
            assert_eq!(
                graph
                    .edges
                    .iter()
                    .any(|e| e.source == external && &e.target == target && e.relation == "calls"),
                visibility == "public",
                "{visibility} {name}"
            );
        }
        assert!(graph.edges.iter().any(|e| e.source == external
            && e.target == node(&base, "exposed").id
            && e.relation == "calls"));
        if visibility != "public" {
            let unresolved = store
                .query("external", &QueryOptions::default())
                .unwrap()
                .unresolved;
            for name in ["work", "token", "token="] {
                assert!(
                    unresolved
                        .iter()
                        .any(|r| r.source == external && r.label == format!("worker.{name}"))
                );
            }
        }
    }
}

#[test]
fn ruby_named_visibility_overrides_preserve_public_overrides_and_reject_dynamics() {
    let base = facts(
        "base.rb",
        "class Base\n def work; :base; end\n def self.report; :base; end\nend",
    );
    let app = facts(
        "app.rb",
        "def external\n worker = Child.new\n worker.work\n Child.report\nend",
    );
    let caller = node(&app, "external").id.clone();
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&tmp.path().join("ruby.db")).unwrap();
    store
        .apply_native(
            "fixture",
            vec![base.clone(), app],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    for directive in [
        "private :work\n private_class_method :report",
        "public :work\n public_class_method :report",
        "private choose_name()\n private_class_method choose_name()",
    ] {
        let child = facts(
            "child.rb",
            &format!("class Child < Base\n {directive}\nend"),
        );
        store
            .apply_native("fixture", vec![child], vec![], Coverage::default())
            .unwrap();
        let graph = store.snapshot().unwrap();
        for name in ["work", "report"] {
            assert_eq!(
                graph.edges.iter().any(|e| e.source == caller
                    && e.target == node(&base, name).id
                    && e.relation == "calls"),
                directive.starts_with("public"),
                "{directive}: {name}"
            );
        }
    }
    let private_base = facts(
        "base.rb",
        "class Base\n private\n def work; :secret; end\n def self.report; :public; end\nend",
    );
    let public_child = facts("child.rb", "class Child < Base\n public :work\nend");
    store
        .apply_native(
            "fixture",
            vec![private_base.clone(), public_child],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    for name in ["work", "report"] {
        assert!(
            store
                .snapshot()
                .unwrap()
                .edges
                .iter()
                .any(|e| e.source == caller
                    && e.target == node(&private_base, name).id
                    && e.relation == "calls")
        );
    }
    let override_child = facts(
        "child.rb",
        "class Child < Base\n private :work\n def work; :public_override; end\nend",
    );
    store
        .apply_native(
            "fixture",
            vec![override_child.clone()],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    assert!(
        store
            .snapshot()
            .unwrap()
            .edges
            .iter()
            .any(|e| e.source == caller
                && e.target == node(&override_child, "work").id
                && e.relation == "calls")
    );
}
