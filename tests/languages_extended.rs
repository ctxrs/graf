use graf::{
    languages::{parse, supports},
    model::{FileFacts, Node},
};
fn facts(path: &str, source: &str) -> FileFacts {
    let f = parse(path, source, "hash").unwrap().unwrap();
    assert!(f.diagnostics.is_empty(), "{path}: {:?}", f.diagnostics);
    f
}
fn node<'a>(f: &'a FileFacts, name: &str) -> &'a Node {
    f.nodes
        .iter()
        .find(|n| n.label == name)
        .unwrap_or_else(|| panic!("missing {name}: {:?}", f.nodes))
}
fn has_call(f: &FileFacts, name: &str) -> bool {
    f.references
        .iter()
        .any(|r| r.relation == "calls" && r.label == name)
}
fn key(f: &FileFacts, name: &str) -> String {
    node(f, name).binding_key.clone().unwrap()
}

#[test]
fn scala_definitions_imports_heritage_and_shadowing() {
    let f = facts(
        "a.scala",
        "package demo\nimport other.Helper\nclass Base\nclass Thing extends Base { def run(x: Int): Int = helper(x) }\ndef helper(x: Int) = x\ndef outer(helper: Int => Int) = helper(1)\n// def fake() = ghost()\n",
    );
    node(&f, "Thing");
    node(&f, "run");
    assert!(has_call(&f, "helper"));
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "inherits" && r.candidate_keys.contains(&key(&f, "Base")))
    );
    let outer = node(&f, "outer");
    assert!(
        f.references
            .iter()
            .filter(|r| r.source == outer.id && r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(!f.nodes.iter().any(|n| n.label == "fake"));
}
#[test]
fn dart_relative_namespace_calls_and_parts() {
    let lib = facts("lib/helper.dart", "int helper(int x) => x;\n");
    let f = facts(
        "lib/main.dart",
        "import 'helper.dart' as lib;\npart 'piece.dart';\nclass Thing extends Base implements Face { int run(int x) { return lib.helper(x); } }\n// class Fake {}\n",
    );
    node(&f, "Thing");
    node(&f, "run");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.candidate_keys.contains(&key(&lib, "helper")))
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "imports" && r.label == "piece.dart")
    );
    assert!(!f.nodes.iter().any(|n| n.label == "Fake"));
}
#[test]
fn objective_c_selectors_header_sniff_and_dynamic_messages() {
    let f = facts(
        "thing.m",
        "#import \"Base.h\"\n@interface Thing : Base\n- (void)run:(int)x with:(int)y;\n@end\n@implementation Thing\n- (void)run:(int)x with:(int)y { [self helper:x]; }\n@end\n",
    );
    node(&f, "Thing");
    node(&f, "-run:with:");
    assert!(has_call(&f, "helper:"));
    assert!(
        f.references
            .iter()
            .filter(|r| r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(
        parse("numbers.m", "function y = f(x)\ny = x + 1;\nend\n", "h")
            .unwrap()
            .is_none()
    );
    assert!(
        parse("comment.m", "/* @interface Fake */\n", "h")
            .unwrap()
            .is_none()
    );
    assert!(
        facts("thing.h", "@interface Thing\n@end\n")
            .nodes
            .iter()
            .any(|n| n.label == "Thing")
    );
}
#[test]
fn julia_modules_short_functions_inheritance_and_quotes() {
    let f = facts(
        "demo.jl",
        "module Demo\nabstract type Base end\nstruct Thing <: Base\n value::Int\nend\nfunction helper(x)\n x\nend\nrun(x) = helper(x)\nquoted = :(ghost())\nend\n",
    );
    node(&f, "Demo");
    node(&f, "Thing");
    node(&f, "run");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.candidate_keys.contains(&key(&f, "helper")))
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "inherits" && r.candidate_keys.contains(&key(&f, "Base")))
    );
    assert!(!has_call(&f, "ghost"));
}
#[test]
fn fortran_case_folded_module_imports_and_calls() {
    let lib = facts(
        "lib.f90",
        "module Lib\ncontains\nsubroutine Helper(x)\ninteger :: x\nend subroutine\nend module\n",
    );
    let f = facts(
        "main.F90",
        "module Demo\nuse LIB, only: HELPER\ncontains\nsubroutine Run(x)\ninteger :: x\ncall helper(x)\nend subroutine\nend module\n! call ghost()\n",
    );
    node(&f, "run");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.candidate_keys.contains(&key(&lib, "helper")))
    );
    assert!(!has_call(&f, "ghost"));
}
#[test]
fn ocaml_modules_variants_interface_and_unbound_receiver() {
    let f = facts(
        "demo.ml",
        "open Other\nmodule Demo = struct\ntype thing = One | Two\nlet helper x = x\nlet run x = helper x\nend\nlet outer x = Demo.run x\nlet unknown x = Other.run x\n",
    );
    node(&f, "One");
    node(&f, "Two");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.candidate_keys.contains(&key(&f, "run")))
    );
    assert!(
        f.references
            .iter()
            .filter(|r| r.label == "Other.run")
            .all(|r| r.candidate_keys
                == ["ocaml:symbol:@other.ml.run", "ocaml:symbol:@other.mli.run"])
    );
    let intf = facts("demo.mli", "module Demo : sig\nval run : int -> int\nend\n");
    node(&intf, "run");
    assert!(!intf.references.iter().any(|r| r.relation == "calls"));
}
#[test]
fn pascal_unit_method_body_and_case_insensitive_call() {
    let f = facts(
        "demo.pas",
        "unit Demo;\ninterface\nuses Other;\ntype Thing = class(Base)\n procedure Run(x: Integer);\nend;\nimplementation\nprocedure Helper(x: Integer); begin end;\nprocedure Thing.Run(x: Integer); begin HELPER(x); end;\nend.\n",
    );
    node(&f, "demo");
    node(&f, "thing");
    node(&f, "thing.run");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.candidate_keys.contains(&key(&f, "helper")))
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "imports" && r.label == "other")
    );
}
#[test]
fn common_lisp_package_class_functions_and_quoted_data() {
    let f = facts(
        "demo.lisp",
        "(defpackage :demo (:use :cl :other))\n(in-package :demo)\n(defclass thing (base) ())\n(defun helper (x) x)\n(defun run (x) (helper x))\n(quote (ghost x))\n'(phantom x)\n",
    );
    node(&f, "thing");
    node(&f, "helper");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.candidate_keys.contains(&key(&f, "helper")))
    );
    assert!(!has_call(&f, "ghost"));
    assert!(!has_call(&f, "phantom"));
}
#[test]
fn verilog_package_class_function_and_module_instantiation() {
    let f = facts(
        "demo.sv",
        "package demo;\nclass Thing extends Base;\nfunction int run(int x); return helper(x); endfunction\nendclass\nendpackage\nmodule top(); Child child(); endmodule\n// module Fake(); endmodule\n",
    );
    node(&f, "demo");
    node(&f, "Thing");
    node(&f, "run");
    node(&f, "top");
    assert!(has_call(&f, "helper"));
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "instantiates" && r.label == "Child")
    );
    assert!(!f.nodes.iter().any(|n| n.label == "Fake"));
}

#[test]
fn verilog_local_instantiation_uses_one_definition_in_either_order() {
    use graf::{
        model::{Coverage, Direction, QueryOptions},
        store::Store,
    };

    let buffer = "module Buffer(); endmodule\n";
    let board = "module Board();\n  Buffer segment();\n  AbsentBuffer missing();\nendmodule\n";
    for (source, local_line, absent_line) in [
        (format!("{buffer}{board}"), 3, 4),
        (format!("{board}{buffer}"), 2, 3),
    ] {
        let file = facts("board.sv", &source);
        let board_id = node(&file, "Board").id.clone();
        let buffer_id = node(&file, "Buffer").id.clone();
        assert_eq!(file.nodes.iter().filter(|n| n.label == "Buffer").count(), 1);
        assert_eq!(key(&file, "Buffer"), "verilog:symbol:Buffer");
        assert!(!file.nodes.iter().any(|n| n.label == "AbsentBuffer"));
        let references: Vec<_> = file
            .references
            .iter()
            .filter(|r| r.relation == "instantiates")
            .collect();
        assert_eq!(references.len(), 2);
        for (label, line) in [("Buffer", local_line), ("AbsentBuffer", absent_line)] {
            let reference = references.iter().find(|r| r.label == label).unwrap();
            assert_eq!(reference.source, board_id);
            assert_eq!(reference.line, line);
            assert_eq!(
                reference.candidate_keys,
                [format!("verilog:symbol:{label}")]
            );
        }
        let reference_id = references
            .iter()
            .find(|r| r.label == "Buffer")
            .unwrap()
            .id
            .clone();
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
        store
            .apply_native("fixture", vec![file], vec![], Coverage::default())
            .unwrap();
        let result = store
            .neighbors(
                &board_id,
                &QueryOptions {
                    direction: Direction::Outgoing,
                    relation: Some("instantiates".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!result.truncated);
        assert_eq!(result.edges.len(), 1);
        let edge = &result.edges[0];
        assert_eq!((&edge.source, &edge.target), (&board_id, &buffer_id));
        assert_eq!(edge.relation, "instantiates");
        assert_eq!(edge.file.as_deref(), Some("board.sv"));
        assert_eq!(edge.line, Some(local_line));
        assert_eq!(edge.metadata["reference_id"], reference_id);
        assert_eq!(result.unresolved.len(), 1);
        let absent = &result.unresolved[0];
        assert_eq!(
            (
                &absent.source,
                absent.label.as_str(),
                absent.file.as_str(),
                absent.line
            ),
            (&board_id, "AbsentBuffer", "board.sv", absent_line)
        );
        let graph = store.snapshot().unwrap();
        assert_eq!(
            graph.nodes.iter().filter(|n| n.label == "Buffer").count(),
            1
        );
        assert!(!graph.nodes.iter().any(|n| n.label == "AbsentBuffer"));
    }
}

#[test]
fn verilog_provider_removal_and_duplicate_definitions_leave_instantiation_unresolved() {
    use graf::{
        model::{Coverage, Direction, QueryOptions},
        store::Store,
    };

    let caller = facts(
        "board.sv",
        "module Board();\n  Buffer segment();\nendmodule\n",
    );
    let board = node(&caller, "Board").id.clone();
    assert!(!caller.nodes.iter().any(|n| n.label == "Buffer"));
    let reference = caller
        .references
        .iter()
        .find(|r| r.relation == "instantiates")
        .unwrap()
        .clone();
    assert_eq!(reference.candidate_keys, ["verilog:symbol:Buffer"]);
    let provider = facts("buffer.sv", "module Buffer(); endmodule\n");
    let buffer = node(&provider, "Buffer").id.clone();
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::create(&directory.path().join("graph.db")).unwrap();
    let check = |store: &Store, target: Option<&str>, ambiguous: bool| {
        let result = store
            .neighbors(
                &board,
                &QueryOptions {
                    direction: Direction::Outgoing,
                    relation: Some("instantiates".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!result.truncated);
        if let Some(target) = target {
            assert_eq!(result.edges.len(), 1);
            assert_eq!(result.edges[0].source, board);
            assert_eq!(result.edges[0].target, target);
            assert_eq!(result.edges[0].line, Some(2));
            assert_eq!(result.edges[0].metadata["reference_id"], reference.id);
            assert!(result.unresolved.is_empty());
        } else {
            assert!(result.edges.is_empty());
            assert_eq!(result.unresolved.len(), 1);
            let unresolved = &result.unresolved[0];
            assert_eq!(
                (
                    &unresolved.source,
                    unresolved.label.as_str(),
                    unresolved.file.as_str(),
                    unresolved.line
                ),
                (&board, "Buffer", "board.sv", 2)
            );
            assert_eq!(unresolved.reason.contains("ambiguous binding"), ambiguous);
        }
        assert!(
            !store
                .snapshot()
                .unwrap()
                .nodes
                .iter()
                .any(|n| n.label == "Buffer" && n.file == "board.sv")
        );
    };
    store
        .apply_native("fixture", vec![caller], vec![], Coverage::default())
        .unwrap();
    check(&store, None, false);
    store
        .apply_native("fixture", vec![provider], vec![], Coverage::default())
        .unwrap();
    check(&store, Some(&buffer), false);
    store
        .apply_native(
            "fixture",
            vec![facts("duplicate.sv", "module Buffer(); endmodule\n")],
            vec![],
            Coverage::default(),
        )
        .unwrap();
    check(&store, None, true);
    assert_eq!(
        store
            .snapshot()
            .unwrap()
            .nodes
            .iter()
            .filter(|n| n.label == "Buffer")
            .count(),
        2
    );
    store
        .apply_native(
            "fixture",
            vec![],
            vec!["duplicate.sv".into()],
            Coverage::default(),
        )
        .unwrap();
    check(&store, Some(&buffer), false);
    store
        .apply_native(
            "fixture",
            vec![],
            vec!["buffer.sv".into()],
            Coverage::default(),
        )
        .unwrap();
    check(&store, None, false);
    assert!(
        !store
            .snapshot()
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.label == "Buffer")
    );
}

#[test]
fn zig_container_function_and_imported_call() {
    let lib = facts("lib.zig", "pub fn helper(x: i32) i32 { return x; }\n");
    let f = facts(
        "main.zig",
        "const lib = @import(\"lib.zig\");\nconst Thing = struct { value: i32, pub fn run(x: i32) i32 { return lib.helper(x); } };\nconst text = \"fn ghost() {}\";\n",
    );
    node(&f, "Thing");
    node(&f, "run");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.candidate_keys.contains(&key(&lib, "helper")))
    );
    assert!(!f.nodes.iter().any(|n| n.label == "ghost"));
}
#[test]
fn apex_class_trigger_and_unknown_receiver() {
    let f = facts(
        "demo.cls",
        "public class Thing extends Base implements Face { public Integer run(Integer x) { return receiver.helper(x); } }\ntrigger Changed on Account (before insert) {}\n",
    );
    node(&f, "Thing");
    node(&f, "run");
    node(&f, "Changed");
    assert!(has_call(&f, "helper"));
    assert!(
        f.references
            .iter()
            .filter(|r| r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "uses" && r.label == "Account")
    );
}
#[test]
fn groovy_class_imports_and_string_false_positive() {
    let f = facts(
        "demo.groovy",
        "package demo\nimport other.Helper\nclass Thing extends Base { int run(int x) { return helper(x); } }\nString text = 'class Fake {}';\n",
    );
    node(&f, "Thing");
    node(&f, "run");
    assert!(has_call(&f, "helper"));
    assert!(!f.nodes.iter().any(|n| n.label == "Fake"));
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "imports" && r.label == "other.Helper")
    );
}
#[test]
fn dm_types_procs_includes_and_instantiation() {
    let f = facts(
        "demo.dm",
        "#include \"other.dm\"\n/obj/thing\n    proc/run(x)\n        helper(x)\n/proc/helper(x)\n    return x\n/obj/thing/proc/extra()\n    new /obj/other()\n",
    );
    node(&f, "/obj/thing");
    node(&f, "/obj/thing/run");
    node(&f, "/proc/helper");
    assert!(has_call(&f, "helper"));
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "instantiates" && r.label == "/obj/other")
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "imports" && r.label == "other.dm")
    );
}
#[test]
fn dm_map_ignores_override_values_and_grid_and_form_owns_controls() {
    let f = facts(
        "map.dmm",
        "\"a\" = (/obj/thing{icon_state = \"/obj/fake\"; other = /obj/hidden}, /turf/floor)\n(1,1,1) = {\"\na\n\"}\n",
    );
    let labels: Vec<_> = f.references.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, vec!["/obj/thing", "/turf/floor"]);
    let f = facts(
        "skin.dmf",
        "window \"main\"\n elem \"button\"\n type = BUTTON\n",
    );
    assert_eq!(node(&f, "button").metadata["control_type"], "BUTTON");
    assert!(
        f.edges
            .iter()
            .any(|e| e.source == node(&f, "main").id && e.target == node(&f, "button").id)
    );
}
#[test]
fn extended_dispatch_locations_and_file_keys_do_not_collide() {
    for ext in [
        "scala", "sc", "dart", "m", "mm", "jl", "f", "F", "f90", "F90", "f95", "F95", "f03", "F03",
        "f08", "F08", "ml", "mli", "pas", "pp", "dpr", "dpk", "lpr", "inc", "lisp", "cl", "lsp",
        "asd", "v", "sv", "svh", "zig", "cls", "trigger", "groovy", "gvy", "gy", "gsh", "gradle",
        "dm", "dme", "dmm", "dmf", "dmi",
    ] {
        assert!(supports(&format!("a.{ext}")), "{ext}");
    }
    let a = facts("left/a.zig", "pub fn helper() void {}\n");
    let b = facts("right/a.zig", "pub fn helper() void {}\n");
    assert_ne!(key(&a, "helper"), key(&b, "helper"));
    let f = facts("pos.scala", "// π\ndef helper() = 1\n");
    let n = node(&f, "helper");
    assert_eq!(n.line, Some(2));
    assert_eq!(n.metadata["start_byte"], 6);
    let bad = parse("bad.dart", "class {", "h").unwrap().unwrap();
    assert!(bad.nodes.is_empty());
    assert!(!bad.diagnostics.is_empty());
}

#[test]
fn dmi_png_metadata_states_and_bounded_compressed_text() {
    fn png(description: &str) -> Vec<u8> {
        let mut bytes = vec![];
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_ztxt_chunk("Description".into(), description.into())
                .unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0]).unwrap();
        }
        bytes
    }
    let f = graf::languages::extended::parse_dmi("icon.dmi", &png("# BEGIN DMI\nversion = 4.0\nwidth = 32\nheight = 32\nstate = \"idle\"\nframes = 2\ndirs = 4\n# END DMI\n"), "h").unwrap();
    assert!(f.diagnostics.is_empty());
    assert_eq!(node(&f, "idle").metadata["frames"], 2);
    let f =
        graf::languages::extended::parse_dmi("icon.dmi", &png(&"x".repeat(256 * 1024 + 1)), "h")
            .unwrap();
    assert!(f.nodes.is_empty());
    assert!(!f.diagnostics.is_empty());
    let f = graf::languages::extended::parse_dmi("icon.dmi", b"not png", "h").unwrap();
    assert!(f.nodes.is_empty());
    assert!(!f.diagnostics.is_empty());
}

#[test]
fn lambda_parameters_and_lisp_package_switches_do_not_cross_bind() {
    let f = facts(
        "main.dart",
        "int helper(int x) => x;\nvoid run() { var callback = (helper) => helper(1); }\n",
    );
    assert!(
        f.references
            .iter()
            .filter(|r| r.relation == "calls" && r.label == "helper")
            .all(|r| r.candidate_keys.is_empty())
    );
    let f = facts(
        "packages.lisp",
        "(in-package :one)\n(defun helper (x) x)\n(in-package :two)\n(defun run (x) (helper x))\n",
    );
    assert!(
        f.references
            .iter()
            .filter(|r| r.relation == "calls" && r.label == "helper")
            .all(|r| r.candidate_keys.is_empty())
    );
    let f = facts(
        "order.ml",
        "let early x = later x\nlet later x = x\nlet number = 1\n",
    );
    assert_eq!(node(&f, "number").kind, "variable");
    assert!(
        f.references
            .iter()
            .filter(|r| r.relation == "calls" && r.label == "later")
            .all(|r| r.candidate_keys.is_empty())
    );
}

#[test]
fn scala_distinguishes_mixins_and_fields_and_apex_tracks_query_objects() {
    let f = facts(
        "types.scala",
        "class Base\ntrait Mix\nclass Thing(val item: Base) extends Base with Mix { val field: Base = item }\n",
    );
    node(&f, "item");
    node(&f, "field");
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "mixes_in" && r.label == "Mix")
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "inherits" && r.label == "Base")
    );
    let f = facts(
        "Query.cls",
        "public class Query { public void run() { List<Account> xs = [SELECT Id FROM Account]; insert new Account(Name='a'); } }",
    );
    assert_eq!(
        f.references
            .iter()
            .filter(|r| r.relation == "uses" && r.label == "Account")
            .count(),
        2
    );
    let f = facts(
        "custom.lisp",
        "(definline helper (x) x)\n(defun run (x) (helper x))\n(default-value phantom)\n",
    );
    assert_eq!(node(&f, "helper").kind, "function");
    assert!(!f.nodes.iter().any(|n| n.label == "phantom"));
}

#[test]
fn lisp_escaped_symbol_case_is_not_folded() {
    let f = facts(
        "names.lisp",
        "(defun |Helper| (x) x)\n(defun run (x) (|helper| x))\n",
    );
    assert!(
        f.references
            .iter()
            .filter(|r| r.relation == "calls" && r.label == "|helper|")
            .all(|r| r.candidate_keys.is_empty())
    );
}

#[test]
fn pascal_strings_and_dm_receiver_calls_do_not_create_targets() {
    let f = facts(
        "literal.pas",
        "program Demo; begin Writeln('Phantom()'); end.\n",
    );
    assert!(!has_call(&f, "phantom"));
    let f = facts(
        "receiver.dm",
        "/proc/helper()\n    return 1\n/obj/thing/proc/run(other)\n    other.helper()\n",
    );
    let references: Vec<_> = f
        .references
        .iter()
        .filter(|r| r.relation == "calls" && r.label == "helper")
        .collect();
    assert_eq!(references.len(), 1);
    assert!(references[0].candidate_keys.is_empty());
}

#[test]
fn pascal_forms_preserve_component_instances_properties_and_events() {
    for extension in ["lfm", "dfm"] {
        let f = facts(
            &format!("ui/main.{extension}"),
            "object Main: TMainForm\n  Caption = 'Not an object Fake: TFake'\n  Width = 640\n  OnCreate = FormCreate\n  object Panel: TPanel\n    inherited ButtonA: TButton\n      Caption = 'It''s ready'\n      OnClick = ButtonClick\n      Action = SaveAction\n    end\n    inline ButtonB: TButton\n      Tag = 2\n    end\n  end\n  object SaveAction: TAction\n  end\nend\n",
        );
        let main = node(&f, "TMainForm");
        assert_eq!(main.metadata["properties"]["Width"], 640);
        assert_eq!(main.metadata["name"], "Main");
        let buttons: Vec<_> = f.nodes.iter().filter(|n| n.label == "TButton").collect();
        assert_eq!(buttons.len(), 2);
        assert_ne!(buttons[0].id, buttons[1].id);
        assert_eq!(buttons[0].metadata["properties"]["Caption"], "It's ready");
        assert!(
            f.edges
                .iter()
                .any(|e| e.source == node(&f, "TPanel").id && e.target == buttons[0].id)
        );
        assert!(f.references.iter().any(|r| r.source == buttons[0].id
            && r.label == "ButtonClick"
            && r.reason.contains("OnClick")
            && r.candidate_keys.is_empty()));
        assert!(
            f.references
                .iter()
                .any(|r| r.label == "SaveAction" && r.candidate_keys.contains(&key(&f, "TAction")))
        );
        assert!(!f.nodes.iter().any(|n| n.label == "TFake"));
    }
}

#[test]
fn pascal_form_collections_strings_comments_and_binary_are_not_components() {
    let f = facts(
        "ui.dfm",
        "object Main: TMain\n  Items.Strings = (\n    'object Fake: TFake'\n    'end')\n  Columns = <\n    item\n      Caption = 'OnClick = Ghost'\n    end>\n  Bitmap.Data = {\n    454E44\n  }\n  // object Fake: TFake\n  OnShow = ShowForm\nend\n",
    );
    assert_eq!(f.nodes.iter().filter(|n| n.kind == "component").count(), 1);
    assert!(!f.references.iter().any(|r| r.label == "Ghost"));
    for bytes in [&b"TPF0binary"[..], &b"\xff\x0a\x00broken"[..]] {
        let f = graf::languages::extended::parse_pascal_form_bytes("ui.dfm", bytes, "h").unwrap();
        assert!(f.nodes.is_empty());
        assert!(
            f.diagnostics
                .iter()
                .any(|d| d.message.contains("Binary DFM"))
        );
    }
    let f = parse("bad.lfm", "object Main: TMain\n", "h")
        .unwrap()
        .unwrap();
    assert!(f.nodes.is_empty());
    assert!(!f.diagnostics.is_empty());
}

#[test]
fn lazarus_package_units_use_lexical_paths_and_external_paths_stay_unresolved() {
    let f = facts(
        "packages/demo.lpk",
        r#"<?xml version="1.0"?>
<CONFIG><Package><Name Value="Demo"/><RequiredPkgs Count="1"><Item1><PackageName Value="Widgets"/></Item1></RequiredPkgs>
<Files Count="4">
<Item1><Filename Value="..\src\main.pas"/><UnitName Value="Main"/></Item1>
<Item2><Filename Value="../../outside.pas"/><UnitName Value="Outside"/></Item2>
<Item3><Filename Value="C:\external\external.pas"/><UnitName Value="External"/></Item3>
<Item4><UnitName Value="DeclaredOnly"/></Item4>
</Files></Package></CONFIG>"#,
    );
    let package = node(&f, "Demo");
    assert_eq!(package.kind, "package");
    assert!(
        f.references
            .iter()
            .any(|r| r.source == package.id && r.relation == "imports" && r.label == "Widgets")
    );
    assert!(
        f.edges
            .iter()
            .any(|e| e.source == package.id && e.target == node(&f, "Main").id)
    );
    let find = |name| f.references.iter().find(|r| r.label == name).unwrap();
    assert_eq!(
        find("Main").candidate_keys,
        vec!["pascal:file:src/main.pas"]
    );
    assert!(find("Outside").candidate_keys.is_empty());
    assert!(find("External").candidate_keys.is_empty());
    assert_eq!(
        find("DeclaredOnly").candidate_keys,
        vec!["pascal:symbol:declaredonly"]
    );
    for source in [
        "<CONFIG><Package></CONFIG>",
        "<!DOCTYPE CONFIG [<!ENTITY x SYSTEM 'file:///missing'>]><CONFIG><Package><Name Value='&x;'/></Package></CONFIG>",
    ] {
        let f = parse("bad.lpk", source, "h").unwrap().unwrap();
        assert!(f.nodes.is_empty());
        assert!(!f.diagnostics.is_empty());
    }
}

#[test]
fn delphi_package_lazarus_program_and_pascal_include_parse_actual_syntax() {
    let f = facts(
        "packages/demo.dpk",
        "package Demo;\n{$R *.res}\nrequires Runtime, Widgets;\ncontains Main in '..\\src\\main.pas' {MainForm}, External in '../../outside.pas';\nend.\n",
    );
    assert_eq!(node(&f, "Demo").kind, "package");
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "Runtime" && r.candidate_keys == ["pascal:package:runtime"])
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "Main" && r.candidate_keys == ["pascal:file:src/main.pas"])
    );
    assert!(
        f.references
            .iter()
            .filter(|r| r.label == "External")
            .all(|r| r.candidate_keys.is_empty())
    );
    let f = facts("main.lpr", "program Demo;\nuses SysUtils;\nbegin\nend.\n");
    node(&f, "demo");
    let f = facts("routines.inc", "procedure Run;\nbegin\nend;\n");
    node(&f, "run");
    for ext in ["lfm", "dfm", "lpk", "lpr", "dpk", "inc"] {
        assert!(supports(&format!("a.{ext}")));
    }
}

#[test]
fn lisp_docstrings_have_owners_and_string_initializers_are_not_docs() {
    let f = facts(
        "docs.lisp",
        r#"
(defun describe-value (x) "Describe \"value\" without (ghost)." x)
(defmacro wrap (x) "Wrap a form." x)
(defparameter *banner* "ordinary value")
(defparameter *limit* 4 "Upper bound.")
(defclass widget () () (:documentation "A displayed widget."))
(defun later-string () (print "first") "not documentation")
(quote (defun fake () "not a docstring"))
"#,
    );
    let docs: Vec<_> = f.nodes.iter().filter(|n| n.kind == "rationale").collect();
    assert_eq!(docs.len(), 4, "{docs:?}");
    for (owner, text) in [
        ("describe-value", "Describe \"value\" without (ghost)."),
        ("wrap", "Wrap a form."),
        ("*limit*", "Upper bound."),
        ("widget", "A displayed widget."),
    ] {
        let doc = docs.iter().find(|d| d.metadata["text"] == text).unwrap();
        assert!(f.edges.iter().any(|e| e.source == doc.id
            && e.target == node(&f, owner).id
            && e.relation == "rationale_for"));
    }
    assert!(!has_call(&f, "ghost"));
    assert!(!f.nodes.iter().any(|n| n.label == "fake"));
}

#[test]
fn fortran_preprocessor_retains_static_and_marks_unknown_branches() {
    let f = facts(
        "shapes.F90",
        "#define NDIM 3\n#define LOCAL 1\nmodule Shapes\n#ifdef MPI\nuse mpi\n#endif\ncontains\n#if LOCAL\nsubroutine volume(side, out)\nreal :: side, out\nout = side ** NDIM\nend subroutine\n#else\nsubroutine inactive()\nend subroutine\n#endif\n#ifdef EXTERNAL_FLAG\nsubroutine optional()\ncall optional_work()\nend subroutine\n#endif\n#if 0\nsubroutine disabled()\nend subroutine\n#endif\nend module\n",
    );
    assert!(node(&f, "volume").binding_key.is_some());
    let optional = node(&f, "optional");
    assert_eq!(optional.metadata["conditional_compilation"], true);
    assert!(optional.binding_key.is_none());
    assert!(
        !f.nodes
            .iter()
            .any(|n| matches!(n.label.as_str(), "inactive" | "disabled"))
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "mpi" && r.relation == "imports" && r.candidate_keys.is_empty())
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "optional_work" && r.candidate_keys.is_empty())
    );
    assert!(!has_call(&f, "EXTERNAL_FLAG"));
}

#[test]
fn ocaml_explicit_units_and_aliases_do_not_bind_bare_names_or_other_directories() {
    let lib = facts(
        "lib/geometry.ml",
        "let area x = x\nmodule Nested = struct let run x = x end\n",
    );
    let unrelated = facts("elsewhere/geometry.ml", "let area x = x\n");
    let f = facts(
        "lib/main.ml",
        "open Geometry\nmodule G = Geometry\nlet area x = x\nlet run x = Geometry.area x\nlet alias x = G.Nested.run x\nmodule F (Geometry : sig val area : int -> int end) = struct\nlet dynamic x = Geometry.area x\nend\n",
    );
    let reference = f
        .references
        .iter()
        .find(|r| r.source == node(&f, "run").id && r.label == "Geometry.area")
        .unwrap();
    assert!(reference.candidate_keys.contains(&key(&lib, "area")));
    assert!(!reference.candidate_keys.contains(&key(&f, "area")));
    assert!(!reference.candidate_keys.contains(&key(&unrelated, "area")));
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "G.Nested.run" && r.candidate_keys.contains(&key(&lib, "run")))
    );
    assert!(
        f.references
            .iter()
            .filter(|r| r.source == node(&f, "dynamic").id)
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(f.references.iter().any(|r| {
        r.relation == "imports"
            && r.candidate_keys
                .contains(&"ocaml:file:lib/geometry.ml".into())
    }));
}

#[test]
fn apex_dml_uses_declared_types_and_annotations_stay_on_their_declarations() {
    let f = facts(
        "Records.cls",
        "public class Records {\n@AuraEnabled(cacheable=true) public static void save(Account row, List<Contact> rows, SObject unknown) {\ninsert row; update rows; delete unknown;\n{ SObject row; delete row; }\nupsert row;\n}\n@InvocableMethod public static void invoke(List<Lead> leads) { insert leads; }\npublic static void plain() { String text = '@AuraEnabled insert Account'; }\n}\n",
    );
    let save = node(&f, "save");
    assert!(
        f.references.iter().any(|r| r.source == save.id
            && r.label == "Account"
            && r.reason.contains("dml_insert"))
    );
    assert!(
        f.references.iter().any(|r| r.source == save.id
            && r.label == "Contact"
            && r.reason.contains("dml_update"))
    );
    assert!(
        f.references
            .iter()
            .filter(|r| r.reason.contains("dml_delete"))
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "Account" && r.reason.contains("dml_upsert"))
    );
    for name in ["save", "invoke"] {
        assert!(
            f.edges
                .iter()
                .any(|e| e.relation == "exposes" && e.target == node(&f, name).id)
        );
    }
    assert!(
        !f.edges
            .iter()
            .any(|e| e.relation == "exposes" && e.target == node(&f, "plain").id)
    );
    assert_eq!(f.nodes.iter().filter(|n| n.kind == "annotation").count(), 2);
}

#[test]
fn dart_framework_relationships_follow_imports_types_and_literal_routes() {
    let f = facts(
        "lib/app.dart",
        r#"
import 'package:flutter/widgets.dart';
import 'package:flutter_bloc/flutter_bloc.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:riverpod_annotation/riverpod_annotation.dart';
import 'package:go_router/go_router.dart';
class LoginEvent {}
class LoadingState {}
class AuthBloc extends Bloc<LoginEvent, LoadingState> {
  void configure() { on<LoginEvent>((event, emit) { emit(LoadingState()); }); }
}
final messageProvider = Provider<String>((ref) => 'hello');
@riverpod
String greeting(Ref ref) => ref.watch(messageProvider);
void show(BuildContext context, WidgetRef ref, AuthBloc bloc) {
  BlocBuilder<AuthBloc, LoadingState>(builder: (context, state) => null);
  context.read<AuthBloc>();
  bloc.add(LoginEvent());
  ref.watch(messageProvider);
  context.go('/home?tab=two');
  context.goNamed('home');
  Navigator.pushNamed(context, '/legacy');
  GoRoute(path: '/home?tab=two', name: 'home');
}
"#,
    );
    for (label, context) in [
        ("LoginEvent", "bloc_event"),
        ("LoadingState", "emit_state"),
        ("AuthBloc", "bloc_widget_binding"),
        ("AuthBloc", "bloc_lookup"),
        ("LoginEvent", "bloc_add_event"),
        ("messageProvider", "riverpod_reference"),
    ] {
        assert!(
            f.references.iter().any(|r| r.label == label
                && r.reason == context
                && r.candidate_keys.contains(&key(&f, label))),
            "missing {context}: {:?}",
            f.references
        );
    }
    assert!(node(&f, "greetingProvider").metadata["generated"] == true);
    for route in ["/home?tab=two", "home", "/legacy"] {
        assert!(
            f.edges
                .iter()
                .any(|e| e.relation == "navigates" && e.target == node(&f, route).id)
        );
    }
    assert!(
        f.edges
            .iter()
            .any(|e| e.relation == "defines" && e.target == node(&f, "home").id)
    );
}

#[test]
fn dart_framework_lookalikes_and_interpolation_do_not_invent_relationships() {
    let f = facts(
        "lib/plain.dart",
        r#"
import 'package:go_router/go_router.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
class Pretender { void go(String path) {} void watch(Object value) {} }
void run(Pretender context, Pretender ref, BuildContext actual, String path) {
  context.go('/fake'); ref.watch(unknownProvider);
  actual.go('/items/$path'); actual.go(path);
  const text = "context.go('/in-a-string')";
}
"#,
    );
    assert!(!f.nodes.iter().any(|n| n.kind == "route"));
    assert!(
        !f.references
            .iter()
            .any(|r| r.reason == "riverpod_reference")
    );
    assert!(!f.edges.iter().any(|e| e.relation == "navigates"));
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "navigates" && r.label == "path" && r.candidate_keys.is_empty())
    );
    let no_import = facts(
        "lib/lookalike.dart",
        "class BuildContext {}\nvoid run(BuildContext context) { context.go('/fake'); }\n",
    );
    assert!(!no_import.nodes.iter().any(|n| n.kind == "route"));
}

#[test]
fn dart_local_framework_type_names_and_emit_parameters_shadow_imports() {
    let f = facts(
        "lib/shadow.dart",
        r#"
import 'package:go_router/go_router.dart';
import 'package:flutter_bloc/flutter_bloc.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
void run(BuildContext context, WidgetRef ref) {
  context.go('/fake'); ref.watch(unknownProvider);
}
class BuildContext {}
class WidgetRef {}
class Provider<T> {}
final pretendProvider = Provider<String>();
class Event {}
class State {}
class RealBloc extends Bloc<Event, State> {
  void shadow(void Function(State) emit) { emit(State()); }
  void register() { on<Event>((event, emit) { emit(State()); }); }
}
"#,
    );
    assert!(
        !f.nodes
            .iter()
            .any(|n| n.kind == "route" || n.label == "pretendProvider")
    );
    assert!(
        !f.references
            .iter()
            .any(|r| r.reason == "riverpod_reference")
    );
    assert!(
        !f.references
            .iter()
            .any(|r| r.source == node(&f, "shadow").id && r.reason == "emit_state")
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.source == node(&f, "register").id && r.reason == "emit_state")
    );
}

#[test]
fn groovy_untyped_parameters_preserve_calls_scopes_and_original_ranges() {
    let source = "import tools.Helper\r\ndef helper(int value) { return value }\r\ndef apply(helper, int value, extra) {\r\n  helper(value)\r\n  Helper.clean(extra)\r\n}\r\ndef run(value) { return helper(value) }\r\n";
    let f = facts("scripts/use.groovy", source);
    let apply = node(&f, "apply");
    let run = node(&f, "run");
    assert_eq!(apply.line, Some(3));
    assert_eq!(apply.end_line, Some(6));
    assert_eq!(
        apply.metadata["start_byte"],
        source.find("def apply").unwrap()
    );
    assert_eq!(
        apply.metadata["end_byte"],
        source.find("}\r\ndef run").unwrap() + 1
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.source == apply.id && r.label == "helper" && r.candidate_keys.is_empty())
    );
    assert!(f.references.iter().any(|r| r.source == run.id
        && r.label == "helper"
        && r.candidate_keys.contains(&key(&f, "helper"))));
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "imports" && r.label == "tools.Helper")
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.label == "clean" && r.line == 5)
    );
    let spans = node(&f, "scripts/use").metadata["normalization"]
        .as_array()
        .unwrap();
    assert_eq!(
        spans
            .iter()
            .filter(|s| s["kind"] == "groovy_untyped_parameters")
            .count(),
        2
    );
    let endings: Vec<_> = spans
        .iter()
        .filter(|s| s["kind"] == "groovy_terminal_statement")
        .collect();
    assert_eq!(endings.len(), 2);
    for span in endings {
        let start = span["start_byte"].as_u64().unwrap() as usize;
        let end = span["end_byte"].as_u64().unwrap() as usize;
        assert_eq!(&source[start..end], " ");
        assert!(source[end..].starts_with('}'));
    }
}

#[test]
fn groovy_spock_quoted_features_keep_classes_imports_calls_and_utf8_crlf_offsets() {
    let source = "package examples\r\nimport spock.lang.Specification\r\n// café before the declarations\r\nclass TextSpec extends Specification {\r\n  def setup() { prepare() }\r\n  def \"it's café time\"() {\r\n    given:\r\n    def value = \"café\"\r\n    when:\r\n    def result = value.trim()\r\n    then:\r\n    result == value\r\n  }\r\n  def 'handles #input'(input, expected) {\r\n    expect:\r\n    normalize(input) == expected\r\n    where:\r\n    input | expected\r\n    \"one\" | \"ONE\"\r\n  }\r\n}\r\n";
    let f = facts("specs/TextSpec.groovy", source);
    let class = node(&f, "TextSpec");
    let quoted = node(&f, "\"it's café time\"");
    assert_eq!(quoted.kind, "method");
    assert_eq!(quoted.line, Some(6));
    assert_eq!(quoted.end_line, Some(13));
    assert_eq!(
        quoted.metadata["start_byte"],
        source.find("def \"it's").unwrap()
    );
    assert_eq!(
        quoted.metadata["end_byte"],
        source.find("}\r\n  def 'handles").unwrap() + 1
    );
    let single = node(&f, "'handles #input'");
    assert_eq!(single.line, Some(14));
    assert!(
        f.edges
            .iter()
            .any(|e| e.source == class.id && e.target == quoted.id && e.relation == "contains")
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "imports" && r.label == "spock.lang.Specification")
    );
    assert!(f.references.iter().any(|r| r.relation == "calls"
        && r.label == "trim"
        && r.source == quoted.id
        && r.line == 10));
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "calls" && r.label == "normalize" && r.source == single.id)
    );
    assert!(
        f.edges
            .iter()
            .all(|e| f.nodes.iter().any(|n| n.id == e.source))
    );
    let spans = node(&f, "specs/TextSpec").metadata["normalization"]
        .as_array()
        .unwrap();
    let span = spans
        .iter()
        .find(|v| v["kind"] == "groovy_quoted_method")
        .unwrap();
    let start = span["start_byte"].as_u64().unwrap() as usize;
    let end = span["end_byte"].as_u64().unwrap() as usize;
    assert_eq!(&source[start..end], "\"it's café time\"");
}

#[test]
fn groovy_recovery_ignores_comments_strings_and_method_calls() {
    let f = facts(
        "specs/Safe.groovy",
        r#"
// def "comment feature"() { ghost() }
/* class Fake { def "block feature"(arg) { phantom() } } */
class Safe {
  def "real feature"() {
    def text = "def 'string feature'() { unseen() }"
    def multiline = '''
      def "multiline feature"() { hidden() }
    '''
    receiver.method(1)
  }
}
"#,
    );
    node(&f, "\"real feature\"");
    for name in [
        "Fake",
        "\"comment feature\"",
        "\"block feature\"",
        "'string feature'",
        "\"multiline feature\"",
        "method",
    ] {
        assert!(!f.nodes.iter().any(|n| n.label == name), "invented {name}");
    }
    for name in ["ghost", "phantom", "unseen", "hidden"] {
        assert!(!has_call(&f, name), "invented {name} call");
    }
    assert!(has_call(&f, "method"));
    assert!(
        f.references
            .iter()
            .filter(|r| r.label == "method")
            .all(|r| r.candidate_keys.is_empty())
    );
}

#[test]
fn groovy_recovery_never_accepts_malformed_headers_or_bodies() {
    for source in [
        "class Broken { def \"feature\"(value,, other) { use(value) } }",
        "class Broken { def \"feature\"(value { use(value) } }",
        "class Broken { def \"feature\"() { return ( } }",
        "class Broken { def \"feature\"() { return value + } }",
        "class Broken { def \"feature\"() { use(1) }",
        "class Broken { def \"feature\"() { /* unterminated } }",
        "class Broken { def \"unterminated feature() { use(1) } }",
        "class Broken { def \"feature\"(return) { use(1) } }",
    ] {
        let f = parse("specs/Broken.groovy", source, "h").unwrap().unwrap();
        assert!(!f.diagnostics.is_empty(), "accepted {source}");
        assert!(
            f.nodes.is_empty() && f.edges.is_empty() && f.references.is_empty(),
            "partial facts from {source}: {f:?}"
        );
    }
}

fn linked_extended(mut files: Vec<FileFacts>) -> Vec<FileFacts> {
    let context = graf::languages::extended::ExtendedContext::from_facts(&files);
    for file in &mut files {
        context.apply(file);
    }
    files
}

#[test]
fn objc_explicit_headers_implementations_categories_and_receivers_link_without_merging() {
    let files = linked_extended(vec![
        facts(
            "src/Thing.h",
            "@interface Thing\n+ (void)ready;\n- (void)finish:(id)x with:(id)y;\n@end\n",
        ),
        facts(
            "src/Thing.m",
            "#import \"Thing.h\"\n@implementation Thing\n+ (void)ready {}\n- (void)finish:(id)x with:(id)y {}\n- (void)run:(id)value { [self finish:value with:value]; [value finish:value with:value]; [Thing ready]; }\n@end\n",
        ),
        facts(
            "src/Thing+Extra.h",
            "#import \"Thing.h\"\n@interface Thing (Extra)\n+ (void)extra;\n@end\n",
        ),
        facts(
            "src/Thing+Extra.m",
            "#import \"Thing+Extra.h\"\n@implementation Thing (Extra)\n+ (void)extra { [self ready]; }\n@end\n",
        ),
        facts(
            "src/Caller.m",
            "#import \"Thing+Extra.h\"\n@implementation Caller\n- (void)run { [Thing extra]; }\n@end\n",
        ),
        facts(
            "src/Inheritance.h",
            "@interface Plain\n@end\n@interface Derived : Plain\n@end\n",
        ),
    ]);
    let implementation = &files[1];
    let sends: Vec<_> = implementation
        .references
        .iter()
        .filter(|r| r.relation == "calls" && r.label == "finish:with:")
        .collect();
    assert_eq!(sends.len(), 2);
    assert_eq!(
        sends
            .iter()
            .filter(|r| r.candidate_keys == [key(implementation, "-finish:with:")])
            .count(),
        1
    );
    assert_eq!(
        sends.iter().filter(|r| r.candidate_keys.is_empty()).count(),
        1
    );
    assert!(
        implementation
            .references
            .iter()
            .any(|r| r.label == "ready" && r.candidate_keys == [key(implementation, "+ready")])
    );
    assert!(
        implementation
            .references
            .iter()
            .any(|r| r.relation == "implements"
                && r.source == node(implementation, "Thing").id
                && r.candidate_keys == [key(&files[0], "Thing")])
    );
    assert!(
        files[3]
            .references
            .iter()
            .any(|r| r.relation == "extends" && r.candidate_keys == [key(&files[0], "Thing")])
    );
    assert!(files[4].references.iter().any(|r| r.relation == "calls"
        && r.label == "extra"
        && r.candidate_keys == [key(&files[3], "+extra")]));
    assert!(files[3].references.iter().any(|r| r.relation == "calls"
        && r.label == "ready"
        && r.candidate_keys == [key(implementation, "+ready")]));
    assert_ne!(key(&files[0], "Thing"), key(implementation, "Thing"));
    assert!(
        implementation
            .references
            .iter()
            .any(|r| r.relation == "implements"
                && r.source == node(implementation, "-finish:with:").id
                && r.candidate_keys == [key(&files[0], "-finish:with:")])
    );
    assert!(
        files[5]
            .references
            .iter()
            .any(|r| r.relation == "inherits" && r.candidate_keys == [key(&files[5], "Plain")])
    );
}

#[test]
fn objc_ambiguous_classes_protocol_receivers_and_shadowed_names_stay_unresolved() {
    let files = linked_extended(vec![
        facts("a/Thing.h", "@interface Thing\n+ (void)ready;\n@end\n"),
        facts("b/Thing.h", "@interface Thing\n+ (void)ready;\n@end\n"),
        facts(
            "OnlyProtocol.h",
            "@protocol OnlyProtocol\n+ (void)ready;\n@end\n",
        ),
        facts(
            "Use.m",
            "#import \"a/Thing.h\"\n#import \"b/Thing.h\"\n#import \"OnlyProtocol.h\"\n@implementation Use\n- (void)run { [Thing ready]; [OnlyProtocol ready]; }\n- (void)shadow:(id)Thing { [Thing ready]; }\n@end\n",
        ),
        facts(
            "Shadow.m",
            "#import \"a/Thing.h\"\n@implementation Shadow\n- (void)run:(id)Thing { [Thing ready]; }\n@end\n",
        ),
        facts(
            "Global.m",
            "#import \"a/Thing.h\"\nid Thing;\n@implementation Global\n- (void)run { [Thing ready]; }\n@end\n",
        ),
    ]);
    for file in [&files[3], &files[4], &files[5]] {
        assert!(
            file.references
                .iter()
                .filter(|r| r.relation == "calls")
                .all(|r| r.candidate_keys.is_empty())
        );
    }
    assert_ne!(key(&files[0], "Thing"), key(&files[1], "Thing"));
    let duplicate = linked_extended(vec![
        facts("Thing.h", "@interface Thing\n+ (void)ready;\n@end\n"),
        facts(
            "Thing.m",
            "#import \"Thing.h\"\n@implementation Thing\n+ (void)ready {}\n@end\n",
        ),
        facts(
            "Thing+Override.m",
            "#import \"Thing.h\"\n@implementation Thing (Override)\n+ (void)ready {}\n@end\n",
        ),
        facts(
            "Use.m",
            "#import \"Thing.h\"\n@implementation Use\n- (void)run { [Thing ready]; }\n@end\n",
        ),
    ]);
    assert!(
        duplicate[3]
            .references
            .iter()
            .filter(|r| r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
}

const PASCAL_BASE: &str = "unit Foundation;\ninterface\ntype TBase = class\n procedure Prepare;\nend;\nimplementation\nprocedure TBase.Prepare; begin end;\nend.\n";
const PASCAL_CHILD: &str = "unit Child;\ninterface\nuses Foundation;\ntype TChild = class(TBase)\n procedure Run;\n procedure Shadow(Prepare: TCallback);\nend;\nimplementation\nprocedure TChild.Run; begin Prepare; inherited Prepare; other.Prepare(); end;\nprocedure TChild.Shadow(Prepare: TCallback); begin Prepare(); end;\nend.\n";

#[test]
fn pascal_explicit_uses_and_ancestors_link_written_calls_without_global_name_fallback() {
    let files = linked_extended(vec![
        facts("units/Foundation.pas", PASCAL_BASE),
        facts("units/Child.pas", PASCAL_CHILD),
        facts(
            "unrelated/Other.pas",
            "unit Other;\ninterface\ntype TOther = class\nprocedure Prepare;\nend;\nimplementation\nprocedure TOther.Prepare; begin end;\nend.\n",
        ),
    ]);
    let target = key(&files[0], "tbase.prepare");
    let run = node(&files[1], "tchild.run");
    assert_eq!(
        files[1]
            .references
            .iter()
            .filter(|r| r.source == run.id
                && r.relation == "calls"
                && r.candidate_keys == [target.clone()])
            .count(),
        2
    );
    assert!(
        files[1]
            .references
            .iter()
            .any(|r| r.relation == "inherits" && r.candidate_keys == [key(&files[0], "tbase")])
    );
    assert!(
        files[1]
            .references
            .iter()
            .filter(|r| r.source == node(&files[1], "tchild.shadow").id && r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(
        files[1]
            .references
            .iter()
            .filter(|r| r.label == "other.Prepare" || r.label == "other.prepare")
            .all(|r| r.candidate_keys.is_empty())
    );
    let ambiguous = linked_extended(vec![
        facts("a/Foundation.pas", PASCAL_BASE),
        facts("b/Foundation.pas", PASCAL_BASE),
        facts("Child.pas", PASCAL_CHILD),
    ]);
    assert!(
        ambiguous[2]
            .references
            .iter()
            .filter(|r| r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
    let field_shadow = linked_extended(vec![
        facts("Foundation.pas", PASCAL_BASE),
        facts(
            "Masked.pas",
            "unit Masked;\ninterface\nuses Foundation;\ntype TMasked = class(TBase)\n Prepare: TCallback;\n procedure Run;\nend;\nimplementation\nprocedure TMasked.Run; begin Prepare(); inherited Prepare; end;\nend.\n",
        ),
    ]);
    let calls: Vec<_> = field_shadow[1]
        .references
        .iter()
        .filter(|r| r.relation == "calls")
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls.iter().filter(|r| r.candidate_keys.is_empty()).count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|r| r.candidate_keys == [key(&field_shadow[0], "tbase.prepare")])
            .count(),
        1
    );
}

#[test]
fn pascal_form_events_require_a_unique_matching_sibling_class_and_literal_handler() {
    let code = "unit Main;\ninterface\ntype TMain = class\n procedure Click(Sender: TObject);\nend;\nimplementation\nprocedure TMain.Click(Sender: TObject); begin end;\nend.\n";
    let form = "object Main: TMain\n object Button: TButton\n  OnClick = Click\n  Caption = 'Click'\n end\nend\n";
    let files = linked_extended(vec![
        facts("ui/Main.pas", code),
        facts("ui/Main.dfm", form),
        facts("other/Main.pas", code),
        facts("ui/Wrong.lfm", form),
    ]);
    assert!(
        files[1]
            .references
            .iter()
            .any(|r| r.reason.starts_with("event property")
                && r.candidate_keys == [key(&files[0], "tmain.click")])
    );
    assert_eq!(
        files[1]
            .references
            .iter()
            .filter(|r| !r.candidate_keys.is_empty())
            .count(),
        1
    );
    assert!(
        files[3]
            .references
            .iter()
            .all(|r| r.candidate_keys.is_empty())
    );
    let ambiguous = linked_extended(vec![
        facts("ui/Main.pas", code),
        facts("ui/Main.pp", code),
        facts("ui/Main.dfm", form),
    ]);
    assert!(
        ambiguous[2]
            .references
            .iter()
            .all(|r| r.candidate_keys.is_empty())
    );
}

#[test]
fn lisp_reader_conditionals_retain_definitions_but_never_choose_an_active_feature() {
    let f = facts(
        "features.lisp",
        "#+fast\n(defun choose (x) \"Fast path.\" (work x))\n#-fast\n(defun choose (x) (slow x))\n#+(and unix (not tiny))\n(definline-maybe optimize (x) (work x))\n(defun ordinary () (choose 1))\n'(#-fast (defun quoted () (ghost)))\n#+(defun condition-name () 1)\n(defun guarded () 1)\n",
    );
    let choices: Vec<_> = f.nodes.iter().filter(|n| n.label == "choose").collect();
    assert_eq!(choices.len(), 2);
    assert!(
        choices
            .iter()
            .all(|n| n.binding_key.is_none() && n.metadata["reader_conditions"].is_array())
    );
    assert_eq!(
        node(&f, "optimize").metadata["reader_conditions"][0]["marker"],
        "#+"
    );
    assert_eq!(
        node(&f, "guarded").metadata["reader_conditions"][0]["feature"],
        "(defun condition-name () 1)"
    );
    assert!(
        !f.nodes
            .iter()
            .any(|n| matches!(n.label.as_str(), "quoted" | "condition-name"))
    );
    assert!(!has_call(&f, "ghost"));
    assert!(
        f.references
            .iter()
            .filter(|r| r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
    assert!(node(&f, "ordinary").binding_key.is_some());
}

#[test]
fn extended_context_discovery_uses_only_indexed_inputs_and_fingerprints_membership() {
    use graf::languages::extended::ExtendedContext;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("Foundation.pas"), PASCAL_BASE).unwrap();
    std::fs::write(directory.path().join("Child.pas"), PASCAL_CHILD).unwrap();
    std::fs::write(directory.path().join("Binary.dfm"), b"TPF0\xff").unwrap();
    let partial = ExtendedContext::discover(directory.path(), &["Child.pas".into()]).unwrap();
    let complete = ExtendedContext::discover(
        directory.path(),
        &[
            "Child.pas".into(),
            "Foundation.pas".into(),
            "Binary.dfm".into(),
        ],
    )
    .unwrap();
    assert_ne!(partial.fingerprint(), complete.fingerprint());
    let reordered = ExtendedContext::discover(
        directory.path(),
        &[
            "Binary.dfm".into(),
            "Foundation.pas".into(),
            "Child.pas".into(),
        ],
    )
    .unwrap();
    assert_eq!(complete.fingerprint(), reordered.fingerprint());
    let mut raw = facts("Child.pas", PASCAL_CHILD);
    partial.apply(&mut raw);
    assert!(
        raw.references
            .iter()
            .filter(|r| r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
    let mut linked = facts("Child.pas", PASCAL_CHILD);
    complete.apply(&mut linked);
    assert!(
        linked
            .references
            .iter()
            .any(|r| r.relation == "calls" && !r.candidate_keys.is_empty())
    );
    let count = linked.references.len();
    complete.apply(&mut linked);
    assert_eq!(linked.references.len(), count);
    std::fs::write(
        directory.path().join("Foundation.pas"),
        format!("{PASCAL_BASE}\n{{ revised }}\n"),
    )
    .unwrap();
    let revised = ExtendedContext::discover(
        directory.path(),
        &[
            "Binary.dfm".into(),
            "Foundation.pas".into(),
            "Child.pas".into(),
        ],
    )
    .unwrap();
    assert_ne!(complete.fingerprint(), revised.fingerprint());
    assert!(ExtendedContext::discover(directory.path(), &["../outside.pas".into()]).is_err());
}

#[cfg(unix)]
#[test]
fn extended_context_never_follows_an_indexed_symlink() {
    use graf::languages::extended::ExtendedContext;
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("Foundation.pas"), PASCAL_BASE).unwrap();
    std::os::unix::fs::symlink(outside.path(), directory.path().join("linked")).unwrap();
    assert!(
        ExtendedContext::discover(&directory.path().join("linked"), &["Foundation.pas".into()])
            .is_err()
    );
    std::fs::write(directory.path().join("Child.pas"), PASCAL_CHILD).unwrap();
    let context = ExtendedContext::discover(
        directory.path(),
        &["Child.pas".into(), "linked/Foundation.pas".into()],
    )
    .unwrap();
    let mut child = facts("Child.pas", PASCAL_CHILD);
    context.apply(&mut child);
    assert!(
        child
            .references
            .iter()
            .filter(|r| r.relation == "calls")
            .all(|r| r.candidate_keys.is_empty())
    );
}

#[test]
fn extended_context_rejects_drive_prefixes_in_every_inventory_component() {
    use graf::languages::extended::ExtendedContext;
    let directory = tempfile::tempdir().unwrap();
    for path in [
        "D:Foundation.pas",
        "D:/outside/Foundation.pas",
        "nested/D:Foundation.pas",
        "nested/D:/outside/Foundation.pas",
    ] {
        assert!(
            ExtendedContext::discover(directory.path(), &[path.into()]).is_err(),
            "accepted drive-prefixed path {path}"
        );
    }
    std::fs::create_dir(directory.path().join("nested")).unwrap();
    let source = directory.path().join("nested/Foundation.pas");
    std::fs::write(&source, PASCAL_BASE).unwrap();
    let first =
        ExtendedContext::discover(directory.path(), &["nested/Foundation.pas".into()]).unwrap();
    std::fs::write(&source, format!("{PASCAL_BASE}\n{{ changed }}\n")).unwrap();
    let changed =
        ExtendedContext::discover(directory.path(), &["nested/Foundation.pas".into()]).unwrap();
    assert_ne!(first.fingerprint(), changed.fingerprint());
}

#[test]
fn extended_context_validates_raw_source_hashes_and_unavailable_states() {
    use graf::languages::extended::ExtendedContext;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("Foundation.pas"), PASCAL_BASE).unwrap();
    std::fs::File::create(directory.path().join("Large.pas"))
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    let context = ExtendedContext::discover(
        directory.path(),
        &[
            "Foundation.pas".into(),
            "Missing.pas".into(),
            "Large.pas".into(),
        ],
    )
    .unwrap();
    let hash = blake3::hash(PASCAL_BASE.as_bytes()).to_hex().to_string();
    assert!(context.validate_source("Foundation.pas", &hash).is_ok());
    assert!(
        context
            .validate_source("Large.pas", "oversized:4MiB")
            .is_ok()
    );
    assert!(
        context
            .validate_source("unrelated.rs", "other hash")
            .is_ok()
    );
    assert!(
        context
            .validate_source("Foundation.pas", &format!("{hash}:project"))
            .is_err()
    );
    for path in ["Missing.pas", "Large.pas", "Unindexed.pas"] {
        assert!(
            context.validate_source(path, &hash).is_err(),
            "accepted {path}"
        );
    }
    let changed_source = format!("{PASCAL_BASE}\n{{ changed after discovery }}\n");
    std::fs::write(directory.path().join("Foundation.pas"), &changed_source).unwrap();
    let changed_hash = blake3::hash(changed_source.as_bytes()).to_hex().to_string();
    assert_eq!(
        context
            .validate_source("Foundation.pas", &changed_hash)
            .unwrap_err()
            .to_string(),
        "source changed during context discovery; retry indexing"
    );

    let mut parsed = facts("Foundation.pas", PASCAL_BASE);
    parsed.hash = hash.clone();
    let parsed_context = ExtendedContext::from_facts(&[parsed]);
    assert!(
        parsed_context
            .validate_source("Foundation.pas", &hash)
            .is_ok()
    );
    assert!(
        parsed_context
            .validate_source("Foundation.pas", &changed_hash)
            .is_err()
    );
}

#[test]
fn objc_incomplete_import_closure_never_falls_back_to_sibling_pairing() {
    use graf::{languages::extended::ExtendedContext, model::Reference};

    // Isolate context proof from parsing: imports and class declarations are
    // independent source facts with distinct file and node identities.
    fn input(path: &str, class: Option<(&str, bool)>, import: Option<&str>) -> FileFacts {
        let owner = format!("objc:{path}:module");
        let mut nodes = vec![Node {
            id: owner.clone(),
            label: path.into(),
            kind: "module".into(),
            file: path.into(),
            line: Some(1),
            end_line: Some(1),
            qualified_name: Some(path.into()),
            binding_key: Some(format!("objc:file:{path}")),
            metadata: serde_json::json!({}),
        }];
        if let Some((role, category)) = class {
            nodes.push(Node {
                id: format!("objc:{path}:Thing"),
                label: "Thing".into(),
                kind: "class".into(),
                file: path.into(),
                line: Some(1),
                end_line: Some(1),
                qualified_name: Some("Thing".into()),
                binding_key: Some(format!("fixture:{path}:Thing")),
                metadata: serde_json::json!({
                    "objc_class": "Thing", "objc_role": role, "objc_category": category,
                }),
            });
        }
        FileFacts {
            path: path.into(),
            hash: "fixture".into(),
            module: path.into(),
            nodes,
            edges: vec![],
            references: import
                .into_iter()
                .map(|target| Reference {
                    id: format!("import:{path}"),
                    source: owner.clone(),
                    label: target.into(),
                    relation: "imports".into(),
                    file: path.into(),
                    line: 1,
                    candidate_keys: vec![format!("objc:file:{target}")],
                    reason: "explicit import".into(),
                })
                .collect(),
            diagnostics: vec![],
        }
    }
    fn paired(files: &[FileFacts], relation: &str) -> bool {
        let context = ExtendedContext::from_facts(files);
        let mut source = files[0].clone();
        let mut sibling = files[1].clone();
        context.apply(&mut source);
        context.apply(&mut sibling);
        source.references.iter().any(|r| {
            r.source == node(&source, "Thing").id
                && r.relation == relation
                && r.candidate_keys == [key(&sibling, "Thing")]
        })
    }
    let sibling = input("Thing.h", Some(("class_interface", false)), None);
    let other = input("other/Thing.h", Some(("class_interface", false)), None);
    for (path, category, relation) in [
        ("Thing.m", false, "implements"),
        ("Thing+Extras.m", true, "extends"),
    ] {
        let role = Some(("class_implementation", category));
        assert!(paired(
            &[input(path, role, None), sibling.clone()],
            relation
        ));
        assert!(!paired(
            &[
                input(path, role, Some("other/Thing.h")),
                sibling.clone(),
                other.clone(),
            ],
            relation
        ));

        let mut overflow = vec![
            input(path, role, Some("chain/0.h")),
            sibling.clone(),
            other.clone(),
        ];
        for index in 0..4096 {
            let target = if index == 4095 {
                "other/Thing.h".into()
            } else {
                format!("chain/{}.h", index + 1)
            };
            overflow.push(input(&format!("chain/{index}.h"), None, Some(&target)));
        }
        assert!(!paired(&overflow, relation), "overflow paired {path}");

        // Exactly 4096 distinct files is complete even with a cycle back to
        // the root. Revisiting a file must not consume another traversal slot.
        let mut boundary = vec![input(path, role, Some("chain/0.h")), sibling.clone()];
        for index in 0..4095 {
            let target = if index == 4094 {
                path.into()
            } else {
                format!("chain/{}.h", index + 1)
            };
            boundary.push(input(&format!("chain/{index}.h"), None, Some(&target)));
        }
        assert!(paired(&boundary, relation), "complete closure lost {path}");
    }
}
