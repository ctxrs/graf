use graf::{
    languages::parse,
    model::{FileFacts, Node, Reference},
};

fn facts(path: &str, source: &str) -> FileFacts {
    let facts = parse(path, source, "hash").unwrap().unwrap();
    assert!(facts.diagnostics.is_empty(), "{:?}", facts.diagnostics);
    facts
}

fn node<'a>(facts: &'a FileFacts, label: &str, kind: &str) -> &'a Node {
    facts
        .nodes
        .iter()
        .find(|n| n.label == label && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind} {label}"))
}

fn call<'a>(facts: &'a FileFacts, label: &str) -> &'a Reference {
    let calls: Vec<_> = facts
        .references
        .iter()
        .filter(|r| r.relation == "calls" && r.label == label)
        .collect();
    assert_eq!(calls.len(), 1, "{label}");
    calls[0]
}

#[test]
fn module_and_function_keep_separate_namespaces_in_either_order() {
    for declarations in [
        "mod scan { pub fn collect() {} pub fn gather<T>() {} } fn scan() {}",
        "fn scan() {} mod scan { pub fn collect() {} pub fn gather<T>() {} }",
        "mod r#scan { pub fn collect() {} pub fn gather<T>() {} } fn r#scan() {}",
    ] {
        let source = format!(
            "{declarations} fn run() {{ scan(); scan::collect(); (scan::collect)(); scan::gather::<()>(); }}"
        );
        let f = facts("src/main.rs", &source);
        let function = node(&f, "scan", "function");
        let collect = node(&f, "collect", "function");
        assert_eq!(function.binding_key.as_deref(), Some("rust:.:scan"));
        assert_eq!(call(&f, "scan").candidate_keys, ["rust:.:scan"]);
        assert_eq!(call(&f, "scan").source, node(&f, "run", "function").id);
        for spelling in ["scan::collect", "(scan::collect)"] {
            assert_eq!(
                call(&f, spelling).candidate_keys,
                [collect.binding_key.as_deref().unwrap()],
                "{declarations}: {spelling}"
            );
        }
        assert_eq!(
            call(&f, "scan::gather::<()>").candidate_keys,
            ["rust:.:scan::gather"]
        );
    }
}

#[test]
fn file_module_does_not_erase_a_same_file_value_without_cargo_membership() {
    // Extraction can prove this call without assigning the custom entry point to a crate.
    let f = facts(
        "tools/entry.rs",
        "mod scan; fn run() { scan(); } fn scan() {}",
    );
    let target = node(&f, "scan", "function");
    assert_eq!(
        target.binding_key.as_deref(),
        Some("rust:file/tools/entry:scan")
    );
    assert_eq!(
        call(&f, "scan").candidate_keys,
        [target.binding_key.as_deref().unwrap()]
    );
    assert_eq!(call(&f, "scan").source, node(&f, "run", "function").id);
    assert!(
        f.references.iter().any(|r| r.relation == "imports"
            && r.candidate_keys == ["rust:module:file/tools/entry:scan"])
    );
}

#[test]
fn module_paths_still_supply_types_and_inherent_owners() {
    let f = facts(
        "src/lib.rs",
        "fn shelf() {} mod shelf { pub struct Boxed; } impl shelf::Boxed { fn open(&self) { self.close(); } fn close(&self) {} } fn read(value: shelf::Boxed) { shelf(); }",
    );
    assert_eq!(
        node(&f, "open", "method").metadata["impl_type"],
        "rust:.:shelf::Boxed"
    );
    assert_eq!(
        call(&f, "self.close").candidate_keys,
        ["rust:.:shelf::Boxed::close"]
    );
    assert!(f.references.iter().any(|r| r.relation == "parameter_type"
        && r.label == "shelf::Boxed"
        && r.candidate_keys == ["rust:.:shelf::Boxed"]));
}

#[test]
fn conditional_or_duplicate_modules_do_not_invent_qualified_targets() {
    for declaration in [
        "#[cfg(feature = \"optional\")] mod scan { pub fn collect() {} }",
        "#[cfg(feature = \"optional\")] // stays attached\n/* still attached */ mod scan { pub fn collect() {} }",
        "mod scan; mod scan;",
    ] {
        for source in [
            format!("{declaration} fn scan() {{}} fn run() {{ scan(); scan::collect(); }}"),
            format!("fn scan() {{}} {declaration} fn run() {{ scan(); scan::collect(); }}"),
        ] {
            let f = facts("src/lib.rs", &source);
            assert_eq!(
                node(&f, "scan", "function").binding_key.as_deref(),
                Some("rust:.:scan")
            );
            assert_eq!(call(&f, "scan").candidate_keys, ["rust:.:scan"]);
            assert!(
                call(&f, "scan::collect").candidate_keys.is_empty(),
                "{source}"
            );
        }
    }
}

#[test]
fn namespace_split_preserves_value_shadowing_and_same_namespace_ambiguity() {
    for source in [
        "mod scan; fn scan() {} fn run(scan: fn()) { scan(); }",
        "mod scan; fn scan() {} fn run() { let scan = || {}; scan(); }",
        "mod scan; fn scan() {} fn scan() {} fn run() { scan(); }",
        "mod scan; #[cfg(feature = \"optional\")] fn scan() {} fn run() { scan(); }",
    ] {
        let f = facts("src/lib.rs", source);
        assert!(call(&f, "scan").candidate_keys.is_empty(), "{source}");
    }
    for declaration in [
        "use crate::other as scan;",
        "struct scan;",
        "type scan = ();",
    ] {
        let source = format!("mod scan; {declaration} fn run() {{ scan::collect(); }}");
        let f = facts("src/lib.rs", &source);
        assert!(
            call(&f, "scan::collect").candidate_keys.is_empty(),
            "{source}"
        );
    }
}

#[test]
fn module_qualification_skips_proven_value_only_shadows() {
    let module = "mod scan { pub fn collect() {} }";
    for declarations in [
        "fn run(scan: fn()) { scan(); scan::collect(); }",
        "fn run(r#scan: fn()) { scan(); scan::collect(); }",
        "fn run() { let scan = || {}; scan(); scan::collect(); }",
        "fn run() { let callback = |scan: fn()| { scan(); scan::collect(); }; }",
        "fn run() { let (scan,) = (|| {},); scan(); scan::collect(); }",
        "fn run(mut scan: fn(), other: fn()) { scan = other; scan(); scan::collect(); }",
        "#[cfg(feature = \"optional\")] fn scan() {} fn run() { scan(); scan::collect(); }",
    ] {
        for source in [
            format!("{module} {declarations}"),
            format!("{declarations} {module}"),
        ] {
            let f = facts("src/lib.rs", &source);
            assert!(call(&f, "scan").candidate_keys.is_empty(), "{source}");
            assert_eq!(
                call(&f, "scan::collect").candidate_keys,
                ["rust:.:scan::collect"],
                "{source}"
            );
            assert_eq!(
                node(&f, "collect", "function").binding_key.as_deref(),
                Some("rust:.:scan::collect")
            );
        }
    }
}

#[test]
fn block_local_function_attributes_control_value_namespace_proof() {
    for (attribute, callable, module_available) in [
        ("", true, true),
        ("#[inline]", true, true),
        ("#[cfg(feature = \"optional\")]", false, true),
        ("#[replace]", false, false),
        ("#[cfg_attr(feature = \"optional\", replace)]", false, false),
    ] {
        let declaration = format!("{attribute}\n// attached\n/* still attached */ fn scan() {{}}");
        for body in [
            format!("{declaration} scan(); scan::collect();"),
            format!("scan(); scan::collect(); {declaration}"),
        ] {
            for function in [
                format!("fn run() {{ {body} }}"),
                format!("fn run() {{ {{ {body} }} }}"),
            ] {
                let source = format!("mod scan {{ pub fn collect() {{}} }} {function}");
                let f = facts("src/lib.rs", &source);
                let local = node(&f, "scan", "function");
                if callable {
                    assert_eq!(
                        call(&f, "scan").candidate_keys,
                        [local.binding_key.as_deref().unwrap()],
                        "{source}"
                    );
                } else {
                    assert!(local.binding_key.is_none(), "{source}");
                    assert!(call(&f, "scan").candidate_keys.is_empty(), "{source}");
                }
                if module_available {
                    assert_eq!(
                        call(&f, "scan::collect").candidate_keys,
                        ["rust:.:scan::collect"],
                        "{source}"
                    );
                } else {
                    assert!(
                        call(&f, "scan::collect").candidate_keys.is_empty(),
                        "{source}"
                    );
                }
            }
        }
    }
}

#[test]
fn value_provenance_does_not_hide_type_import_or_module_ambiguity() {
    for declarations in [
        "fn run<scan>(scan: fn()) { scan::collect(); }",
        "fn run<r#scan>(r#scan: fn()) { scan::collect(); }",
        "fn run() { let scan = || {}; type scan = (); scan::collect(); }",
        "fn run() { type scan = (); let scan = || {}; scan::collect(); }",
        "fn run() { let scan = || {}; use crate::other as scan; scan::collect(); }",
        "fn run() { use crate::other as scan; let scan = || {}; scan::collect(); }",
        "use crate::other as scan; fn scan() {} fn run() { scan::collect(); }",
        "fn scan() {} use crate::other as scan; fn run() { scan::collect(); }",
        "#[cfg(feature = \"optional\")] use crate::other as scan; fn scan() {} fn run() { scan::collect(); }",
        "#[replace] fn scan() {} fn run() { scan::collect(); }",
        "#[cfg_attr(feature = \"optional\", replace)] fn scan() {} fn run() { scan::collect(); }",
    ] {
        let source = format!("mod scan {{ pub fn collect() {{}} }} {declarations}");
        let f = facts("src/lib.rs", &source);
        assert!(
            call(&f, "scan::collect").candidate_keys.is_empty(),
            "{source}"
        );
    }
    for module in [
        "#[cfg(feature = \"optional\")] mod scan;",
        "mod scan; mod scan;",
    ] {
        let source = format!("{module} fn run(scan: fn()) {{ scan(); scan::collect(); }}");
        let f = facts("src/lib.rs", &source);
        assert!(call(&f, "scan").candidate_keys.is_empty(), "{source}");
        assert!(
            call(&f, "scan::collect").candidate_keys.is_empty(),
            "{source}"
        );
    }
}

#[test]
fn bare_local_named_uses_match_explicit_self_paths_after_all_declarations() {
    for origin in ["command", "self::command", "r#command"] {
        let import = format!("pub use {origin}::{{Menu as PublicMenu, make as create}};");
        for declarations in [
            format!("mod command; {import}"),
            format!("{import} mod command;"),
        ] {
            let source = format!(
                "{declarations} fn run(value: PublicMenu) -> PublicMenu {{ create(); PublicMenu::new() }} impl PublicMenu {{ fn append(&self) {{ self.finish(); }} fn finish(&self) {{}} }}"
            );
            let f = facts("src/builder/mod.rs", &source);
            for (label, target) in [
                ("Menu as PublicMenu", "rust:.:builder::command::Menu"),
                ("make as create", "rust:.:builder::command::make"),
            ] {
                for relation in ["imports", "reexports"] {
                    let refs: Vec<_> = f
                        .references
                        .iter()
                        .filter(|r| r.label == label && r.relation == relation)
                        .collect();
                    assert_eq!(refs.len(), 1, "{source}: {relation} {label}");
                    assert_eq!(refs[0].candidate_keys, [target], "{source}");
                }
            }
            for (label, target) in [
                ("create", "rust:.:builder::command::make"),
                ("PublicMenu::new", "rust:.:builder::command::Menu::new"),
                ("self.finish", "rust:.:builder::command::Menu::finish"),
            ] {
                assert_eq!(call(&f, label).candidate_keys, [target], "{source}");
            }
            for relation in ["parameter_type", "return_type", "impl_type"] {
                assert!(
                    f.references.iter().any(|r| r.label == "PublicMenu"
                        && r.relation == relation
                        && r.candidate_keys == ["rust:.:builder::command::Menu"]),
                    "{source}: {relation}"
                );
            }
            assert_eq!(
                node(&f, "append", "method").metadata["impl_type"],
                "rust:.:builder::command::Menu"
            );
            assert_eq!(
                node(&f, "PublicMenu", "reexport").metadata["reexport_key"],
                "rust:.:builder::PublicMenu"
            );
        }
    }
}

#[test]
fn bare_local_use_proof_is_scope_specific_and_preserves_value_shadowing() {
    let f = facts(
        "src/lib.rs",
        "mod first { mod command; pub use command::Menu; fn run() { Menu::new(); } } mod second { mod command; pub use command::Menu; fn run() { Menu::new(); } }",
    );
    let mut keys: Vec<_> = f
        .references
        .iter()
        .filter(|r| r.relation == "calls" && r.label == "Menu::new")
        .map(|r| r.candidate_keys.clone())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            vec!["rust:.:first::command::Menu::new"],
            vec!["rust:.:second::command::Menu::new"]
        ]
    );

    let f = facts(
        "src/lib.rs",
        "mod command; use command::{Menu, make as create}; fn run<Menu>(create: fn()) { create(); Menu::new(); }",
    );
    assert!(call(&f, "create").candidate_keys.is_empty());
    assert!(call(&f, "Menu::new").candidate_keys.is_empty());
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "imports" && r.candidate_keys == ["rust:.:command::Menu"])
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.relation == "imports" && r.candidate_keys == ["rust:.:command::make"])
    );

    let f = facts(
        "src/lib.rs",
        "mod command; fn run(command: fn()) { use command::Menu; command(); Menu::new(); }",
    );
    assert!(call(&f, "command").candidate_keys.is_empty());
    assert_eq!(
        call(&f, "Menu::new").candidate_keys,
        ["rust:.:command::Menu::new"]
    );
}

#[test]
fn bare_local_use_requires_unconditional_unambiguous_module_and_binding() {
    for declarations in [
        "",
        "#[cfg(feature = \"optional\")] mod command;",
        "mod command; mod command;",
        "mod command; struct command;",
        "mod command; use crate::other as command;",
        "mod command; #[cfg(feature = \"optional\")] use crate::other as command;",
        "mod command; use crate::other::*;",
        "mod command; #[replace] fn command() {}",
        "mod command; #[cfg_attr(feature = \"optional\", replace)] fn command() {}",
    ] {
        let import =
            "pub use command::Menu as Imported; fn run(value: Imported) { Imported::new(); }";
        for source in [
            format!("{declarations} {import}"),
            format!("{import} {declarations}"),
        ] {
            let f = facts("src/lib.rs", &source);
            let imports: Vec<_> = f
                .references
                .iter()
                .filter(|r| r.label == "command::Menu as Imported")
                .collect();
            assert_eq!(imports.len(), 2, "{source}");
            let expected = if declarations.is_empty() {
                vec!["rust:external:command::Menu"]
            } else {
                vec![]
            };
            assert!(
                imports.iter().all(|r| r.candidate_keys == expected),
                "{source}"
            );
            assert!(
                !f.references
                    .iter()
                    .flat_map(|r| &r.candidate_keys)
                    .any(|key| key.starts_with("rust:.:command::")),
                "{source}"
            );
        }
    }
    for source in [
        "mod command; #[cfg(feature = \"optional\")] pub use command::Menu; fn run(value: Menu) { Menu::new(); }",
        "mod command; pub use command::Menu; use other::Menu; fn run(value: Menu) { Menu::new(); }",
        "mod command; #[cfg(feature = \"optional\")] fn run() { use command::Menu; Menu::new(); }",
        "mod command; #[replace] fn run() { use command::Menu; Menu::new(); }",
        "mod command; fn run<command>() { use command::Menu; Menu::new(); }",
        "mod command; fn run() { use command::Menu; #[replace] fn command() {} Menu::new(); }",
        "mod command; use ::command::Menu; fn run() { Menu::new(); }",
        "mod command; use ::command::{Menu}; fn run() { Menu::new(); }",
    ] {
        let f = facts("src/lib.rs", source);
        assert!(
            !f.references
                .iter()
                .flat_map(|r| &r.candidate_keys)
                .any(|key| key.starts_with("rust:.:command::")),
            "{source}"
        );
    }
}

#[test]
fn blocked_local_imports_cannot_fall_through_to_dependency_candidates() {
    for (declarations, origin) in [
        ("", Some("rust:external:child")),
        ("mod child;", Some("rust:.:child")),
        ("#[cfg(feature = \"alternate\")] mod child;", None),
        ("mod child; mod child;", None),
    ] {
        let imports = "pub use child::{work, Item};";
        for items in [
            format!("{declarations} {imports}"),
            format!("{imports} {declarations}"),
        ] {
            let source =
                format!("{items} pub fn caller(value: Item) -> Item {{ work(); Item::new() }}");
            let f = facts("src/lib.rs", &source);
            for label in ["work", "Item"] {
                let expected: Vec<_> = origin
                    .map(|key| format!("{key}::{label}"))
                    .into_iter()
                    .collect();
                for relation in ["imports", "reexports"] {
                    let refs: Vec<_> = f
                        .references
                        .iter()
                        .filter(|r| r.label == label && r.relation == relation)
                        .collect();
                    assert_eq!(refs.len(), 1, "{source}: {relation} {label}");
                    assert_eq!(refs[0].candidate_keys, expected, "{source}");
                }
            }
            for label in ["work", "Item::new"] {
                let expected: Vec<_> = origin
                    .map(|key| format!("{key}::{label}"))
                    .into_iter()
                    .collect();
                assert_eq!(call(&f, label).candidate_keys, expected, "{source}");
            }
            for relation in ["parameter_type", "return_type"] {
                let expected: Vec<_> = origin
                    .map(|key| format!("{key}::Item"))
                    .into_iter()
                    .collect();
                let refs: Vec<_> = f
                    .references
                    .iter()
                    .filter(|r| r.label == "Item" && r.relation == relation)
                    .collect();
                assert_eq!(refs.len(), 1, "{source}: {relation}");
                assert_eq!(refs[0].candidate_keys, expected, "{source}");
            }
        }
    }
}

#[test]
fn proven_module_aliases_supply_named_uses_in_either_order() {
    for origin in ["command", "self::command"] {
        let alias = format!("pub use {origin} as local;");
        let members = "pub use local::{make as create, Menu as Imported};";
        for imports in [format!("{alias} {members}"), format!("{members} {alias}")] {
            let source = format!(
                "{imports} mod command {{ pub fn make() {{}} pub struct Menu; }} fn run(value: Imported) -> Imported {{ create(); Imported::new() }} impl Imported {{ fn append(&self) {{ self.finish(); }} fn finish(&self) {{}} }}"
            );
            let f = facts("src/lib.rs", &source);
            for (label, target) in [
                (format!("{origin} as local"), "rust:.:command"),
                ("make as create".into(), "rust:.:command::make"),
                ("Menu as Imported".into(), "rust:.:command::Menu"),
            ] {
                for relation in ["imports", "reexports"] {
                    let refs: Vec<_> = f
                        .references
                        .iter()
                        .filter(|r| r.label == label && r.relation == relation)
                        .collect();
                    assert_eq!(refs.len(), 1, "{source}: {relation} {label}");
                    assert_eq!(refs[0].candidate_keys, [target], "{source}");
                }
            }
            assert_eq!(
                call(&f, "create").candidate_keys,
                ["rust:.:command::make"],
                "{source}"
            );
            assert_eq!(
                call(&f, "Imported::new").candidate_keys,
                ["rust:.:command::Menu::new"],
                "{source}"
            );
            assert_eq!(
                call(&f, "self.finish").candidate_keys,
                ["rust:.:command::Menu::finish"],
                "{source}"
            );
            for relation in ["parameter_type", "return_type", "impl_type"] {
                assert!(
                    f.references.iter().any(|r| r.label == "Imported"
                        && r.relation == relation
                        && r.candidate_keys == ["rust:.:command::Menu"]),
                    "{source}: {relation}"
                );
            }
            assert_eq!(
                node(&f, "Imported", "reexport").metadata["reexport_key"],
                "rust:.:Imported"
            );
        }
    }
}

#[test]
fn module_alias_dependencies_keep_scope_and_value_namespace_proof() {
    for origin in ["command", "self::command"] {
        let source = format!(
            "mod outer {{ use relay::make as create; use local as relay; use {origin} as local; mod command {{ pub fn make() {{}} }} #[cfg(feature = \"optional\")] fn command() {{}} fn run(local: fn(), command: fn()) {{ use local::make as inner; inner(); create(); local(); command(); relay::make(); }} }}"
        );
        let f = facts("src/lib.rs", &source);
        for label in ["inner", "create", "relay::make"] {
            assert_eq!(
                call(&f, label).candidate_keys,
                ["rust:.:outer::command::make"],
                "{source}: {label}"
            );
        }
        for label in ["local", "command"] {
            assert!(
                call(&f, label).candidate_keys.is_empty(),
                "{source}: {label}"
            );
        }
    }
    let f = facts(
        "src/lib.rs",
        "mod command; fn run() { struct command; use local::make as create; use self::command as local; create(); }",
    );
    assert_eq!(call(&f, "create").candidate_keys, ["rust:.:command::make"]);
}

#[test]
fn module_alias_proof_rejects_members_external_paths_cycles_and_ambiguity() {
    for (declarations, blocked) in [
        ("mod command; use command::Menu as local;", false),
        ("mod command; use command::nested as local;", false),
        ("struct command; use command as local;", false),
        ("use foreign as local;", false),
        ("mod command; use ::command as local;", false),
        (
            "#[cfg(feature = \"optional\")] mod command; use command as local;",
            true,
        ),
        (
            "#[cfg(feature = \"optional\")] mod command; use self::command as local;",
            true,
        ),
        ("mod command; mod command; use command as local;", true),
        (
            "mod command; #[cfg(feature = \"optional\")] use command as local;",
            true,
        ),
        ("mod command; #[replace] use command as local;", true),
        (
            "#[cfg_attr(feature = \"optional\", replace)] mod command; use command as local;",
            true,
        ),
        (
            "mod command; use command as local; use command as local;",
            true,
        ),
        ("mod command; use command as local; mod local;", true),
        ("mod command; use command as local; struct local;", true),
        ("mod command; use command as local; use other::*;", true),
        (
            "mod command; use command as local; #[replace] fn command() {}",
            true,
        ),
        (
            "mod command; use command as local; use other as command;",
            true,
        ),
        (
            "mod command; use self::command as local; struct command;",
            true,
        ),
        ("use other as local; use local as other;", false),
        ("use local as local;", false),
    ] {
        let downstream = "pub use local::make as create; fn run() { create(); }";
        for source in [
            format!("{declarations} {downstream}"),
            format!("{downstream} {declarations}"),
        ] {
            let f = facts("src/lib.rs", &source);
            let refs: Vec<_> = f
                .references
                .iter()
                .filter(|r| r.label == "local::make as create")
                .collect();
            assert_eq!(refs.len(), 2, "{source}");
            let expected = if blocked {
                vec![]
            } else {
                vec!["rust:external:local::make"]
            };
            assert!(
                refs.iter().all(|r| r.candidate_keys == expected),
                "{source}"
            );
            if blocked {
                assert!(call(&f, "create").candidate_keys.is_empty(), "{source}");
            }
            assert!(
                !call(&f, "create")
                    .candidate_keys
                    .iter()
                    .any(|key| key.starts_with("rust:.:")),
                "{source}"
            );
        }
    }
    for shadow in ["<local>", "<command>"] {
        let source = format!(
            "mod command; use command as local; fn run{shadow}() {{ use local::make as create; create(); }}"
        );
        let f = facts("src/lib.rs", &source);
        // Only the alias head is relevant: a generic named command cannot
        // invalidate module provenance already established for local.
        assert_eq!(
            call(&f, "create").candidate_keys,
            if shadow == "<local>" {
                vec![]
            } else {
                vec!["rust:.:command::make"]
            },
            "{source}"
        );
    }
}

#[test]
fn named_cfg_import_does_not_hide_an_unrelated_inherent_receiver() {
    for import in [
        "#[cfg(debug_assertions)] use crate::checks::verify;",
        "#[cfg(debug_assertions)] // note\n/* another note */ use crate::checks::verify;",
        "#[cfg(feature = \"audit\")] use crate::checks::{verify, deep::check as inspect};",
        "#[cfg(feature = \"audit\")] use crate::checks::{self as audit, verify};",
    ] {
        let source = format!(
            "{import} struct Palette {{}} impl Palette {{ fn append(mut self, value: u8) -> Self {{ self.store(value); verify(); self }} fn store(&mut self, value: u8) {{}} }}"
        );
        let f = facts("src/palette.rs", &source);
        let append = node(&f, "append", "method");
        assert_eq!(
            append.binding_key.as_deref(),
            Some("rust:.:palette::Palette::append"),
            "{import}"
        );
        assert_eq!(append.metadata["impl_type"], "rust:.:palette::Palette");
        assert_eq!(append.metadata["trait_receiver"], true);
        assert_eq!(call(&f, "self.store").source, append.id);
        assert_eq!(
            call(&f, "self.store").candidate_keys,
            ["rust:.:palette::Palette::store"]
        );
        assert_eq!(
            node(&f, "store", "method").binding_key.as_deref(),
            Some("rust:.:palette::Palette::store")
        );
        assert!(call(&f, "verify").candidate_keys.is_empty(), "{import}");
    }
}

#[test]
fn cfg_imports_still_block_each_imported_alias() {
    let f = facts(
        "src/lib.rs",
        "#[cfg(feature = \"audit\")] use crate::checks::{self as audit, deep::{verify as inspect, finish}}; fn run() { audit::start(); inspect(); finish(); }",
    );
    for spelling in ["audit::start", "inspect", "finish"] {
        assert!(call(&f, spelling).candidate_keys.is_empty(), "{spelling}");
    }
}

#[test]
fn unproven_imports_and_impl_headers_do_not_gain_concrete_receivers() {
    for declarations in [
        "#[cfg(feature = \"optional\")] use crate::foreign::Palette; struct Palette {}",
        "use crate::first::Palette; use crate::second::Palette;",
        "#[cfg(feature = \"optional\")] use crate::foreign::*; struct Palette {}",
        "use crate::foreign::*; struct Palette {}",
        "#[replace_import] use crate::checks::verify; struct Palette {}",
        "#[cfg_attr(feature = \"optional\", replace_import)] use crate::checks::verify; struct Palette {}",
    ] {
        let source = format!(
            "{declarations} impl Palette {{ fn append(&self) {{ self.store(); }} fn store(&self) {{}} }}"
        );
        let f = facts("src/lib.rs", &source);
        assert!(
            node(&f, "append", "method").binding_key.is_none(),
            "{declarations}"
        );
        assert!(
            node(&f, "append", "method").metadata["impl_type"].is_null(),
            "{declarations}"
        );
        assert!(
            call(&f, "self.store").candidate_keys.is_empty(),
            "{declarations}"
        );
    }
    for header in ["impl Paint for Palette", "impl<T: Paint> Palette<T>"] {
        let source = format!(
            "#[cfg(debug_assertions)] use crate::checks::verify; {header} {{ fn append(&self) {{ self.store(); }} }}"
        );
        let f = facts("src/lib.rs", &source);
        assert!(
            node(&f, "append", "method").binding_key.is_none(),
            "{header}"
        );
        assert!(call(&f, "self.store").candidate_keys.is_empty(), "{header}");
    }
}
