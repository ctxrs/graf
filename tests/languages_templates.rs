use graf::{languages::parse, model::FileFacts};

fn facts(path: &str, source: &str) -> FileFacts {
    parse(path, source, "fixture").unwrap().unwrap()
}
fn has_ref(f: &FileFacts, relation: &str, label: &str) -> bool {
    f.references
        .iter()
        .any(|r| r.relation == relation && r.label == label)
}

#[test]
fn components_keep_script_locations_and_resolve_handlers() {
    for ext in ["vue", "svelte"] {
        let source = "<!-- π -->\n<script lang=\"ts\" generic=\"T extends Record<string, unknown>\">\nimport Card from './Card.vue';\nimport { save } from './api';\nfunction click(): void { save(); }\n</script>\n<Card @click=\"click\" />\n";
        let f = facts(&format!("ui/Page.{ext}"), source);
        assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
        let click = f.nodes.iter().find(|n| n.label == "click").unwrap();
        assert_eq!(click.line, Some(5));
        let start = click.metadata["start_byte"].as_u64().unwrap() as usize;
        let end = click.metadata["end_byte"].as_u64().unwrap() as usize;
        assert_eq!(&source[start..end], "function click(): void { save(); }");
        let handler = f
            .references
            .iter()
            .find(|r| r.relation == "binds_method")
            .unwrap();
        assert_eq!(
            handler.candidate_keys,
            vec![click.binding_key.clone().unwrap()]
        );
        assert_eq!(handler.line, 7);
        let card = facts("ui/Card.vue", "<template><p>card</p></template>");
        let key = card
            .nodes
            .iter()
            .find(|n| n.kind == "component")
            .unwrap()
            .binding_key
            .as_ref()
            .unwrap();
        assert!(
            f.references
                .iter()
                .any(|r| r.relation == "uses_component" && r.candidate_keys.contains(key))
        );
        assert!(f.nodes.iter().all(|n| n.file == format!("ui/Page.{ext}")));
        assert!(has_ref(&f, "calls", "save"));
    }
}

#[test]
fn astro_frontmatter_and_template_dynamic_imports() {
    let source = "---\nimport Panel from './Panel.astro';\nfunction load() {}\n---\n<Panel />\n{import('./Lazy.astro')}\n<script>import { hydrate } from './client'; hydrate();</script>";
    let f = facts("pages/Home.astro", source);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    assert!(has_ref(&f, "imports", "./Panel.astro"));
    assert!(has_ref(&f, "imports", "./Lazy.astro"));
    assert!(has_ref(&f, "calls", "hydrate"));
    assert_eq!(
        f.nodes.iter().find(|n| n.label == "load").unwrap().line,
        Some(3)
    );
    let f = facts(
        "Page.svelte",
        "{#await import('./Lazy.svelte')}<p>loading</p>{/await}",
    );
    assert!(has_ref(&f, "imports", "./Lazy.svelte"));
}

#[test]
fn blade_references_ignore_comments() {
    let f = facts(
        "resources/views/home.blade.php",
        "{{-- @include('fake') --}}\n@include('shared.menu')\n<livewire:cart wire:click=\"checkout(1)\" />",
    );
    assert!(has_ref(&f, "includes", "shared.menu"));
    assert!(!has_ref(&f, "includes", "fake"));
    assert!(has_ref(&f, "uses_component", "livewire:cart"));
    assert!(has_ref(&f, "binds_method", "checkout"));
    let view = facts("resources/views/shared/menu.blade.php", "<p>Menu</p>");
    assert_eq!(
        f.references
            .iter()
            .find(|r| r.relation == "includes")
            .unwrap()
            .candidate_keys[0],
        view.nodes[0].binding_key.clone().unwrap()
    );
}

#[test]
fn razor_directives_methods_and_real_ranges() {
    let source = "@page \"/orders\"\n@using Shop.Services\n@inject OrderService Orders\n@inherits BasePage\n<OrderCard @onclick=\"Save\" />\n@code {\n private void Save() { var text = \"}\"; Refresh(); }\n private void Refresh() {}\n}\n";
    let f = facts("Pages/Orders.razor", source);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    assert_eq!(f.nodes[0].metadata["route"], "/orders");
    assert!(has_ref(&f, "uses_type", "OrderService"));
    assert!(has_ref(&f, "inherits", "BasePage"));
    assert!(has_ref(&f, "uses_component", "OrderCard"));
    let save = f.nodes.iter().find(|n| n.label == "Save").unwrap();
    assert_eq!(save.line, Some(7));
    assert_eq!(
        &source[save.metadata["start_byte"].as_u64().unwrap() as usize
            ..save.metadata["end_byte"].as_u64().unwrap() as usize],
        "private void Save() { var text = \"}\"; Refresh(); }"
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.source == save.id && r.label == "Refresh" && r.relation == "calls")
    );
    assert_eq!(
        f.references
            .iter()
            .find(|r| r.relation == "binds_method")
            .unwrap()
            .candidate_keys[0],
        save.binding_key.clone().unwrap()
    );
}

#[test]
fn xaml_controls_bindings_resources_events_and_invalid_input() {
    let source = "<Window xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\" xmlns:vm=\"clr-namespace:Shop\" x:Class=\"Shop.Main\">\n<Window.DataContext><vm:MainViewModel /></Window.DataContext>\n<Button x:Name=\"Submit\" Click=\"OnSave\" Command=\"{Binding SaveCommand}\" Content=\"{Binding Name, Converter={StaticResource Format}}\" />\n</Window>";
    let f = facts("Views/Main.xaml", source);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    assert!(
        f.nodes
            .iter()
            .any(|n| n.label == "Submit" && n.line == Some(3))
    );
    assert!(has_ref(&f, "binds_command", "SaveCommand"));
    assert!(has_ref(&f, "uses_resource", "Format"));
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "OnSave" && r.candidate_keys == ["csharp:symbol:Shop.Main.OnSave"])
    );
    assert!(f.references.iter().any(|r| r.label == "vm:MainViewModel"
        && r.candidate_keys == ["csharp:symbol:Shop.MainViewModel"]));
    for source in [
        "<!DOCTYPE Window [<!ENTITY x 'y'>]><Window/>",
        "<Window><Button></Window>",
    ] {
        let f = facts("Bad.xaml", source);
        assert!(f.nodes.is_empty());
        assert!(!f.diagnostics.is_empty());
    }
}

#[test]
fn robot_scoped_calls_imports_bdd_and_templates() {
    let source = "*** Settings ***\nResource    ${Cur_Dir}/steps.resource\nSuite Setup    Prepare\nTest Template    Check Item\n*** Test Cases ***\nTable\n    data is not a keyword\nPlain\n    [Template]    NONE\n    Given Open Session\n    ${value}=    Prepare\n*** Keywords ***\nPrepare\n    Log    ready\n";
    let f = facts("tests/suite.robot", source);
    assert_eq!(f.nodes[0].metadata["coverage"], "static-subset");
    let resource = facts(
        "tests/steps.resource",
        "*** Keywords ***\nOpen Session\n    No Operation\n",
    );
    let key = resource
        .nodes
        .iter()
        .find(|n| n.label == "Open Session")
        .unwrap()
        .binding_key
        .as_ref()
        .unwrap();
    assert!(
        f.references
            .iter()
            .find(|r| r.label == "Given Open Session")
            .unwrap()
            .candidate_keys
            .contains(key)
    );
    assert!(
        f.references.iter().any(|r| r.relation == "imports"
            && r.candidate_keys == ["template:file:tests/steps.resource"])
    );
    assert!(!has_ref(&f, "calls", "data is not a keyword"));
    assert!(has_ref(&f, "calls", "Check Item"));
    assert!(
        f.nodes
            .iter()
            .any(|n| n.label == "Plain" && n.kind == "test")
    );
}

#[test]
fn comments_raw_text_and_unsupported_paths_do_not_fabricate_facts() {
    let f = facts(
        "Page.vue",
        "<!-- <script>function fake(){}</script><Fake/> -->\n<style>.fake { color: red }</style>\n<template>import('./not-a-call.vue')</template>",
    );
    assert!(f.references.is_empty(), "{:?}", f.references);
    assert!(!f.nodes.iter().any(|n| n.label == "fake"));
    assert!(parse("../Page.vue", "", "h").is_err());
    let f = facts("Page.vue", "<script>function broken(</script>");
    assert!(!f.diagnostics.is_empty());
}

#[test]
fn template_bindings_respect_script_reassignment_and_namespace_components() {
    let source = "<script>import {save} from './api'; import * as UI from './widgets'; save = other; export default {}; const text = '<!--';</script><UI.Card @click=\"save\" />";
    let f = facts("Page.vue", source);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    assert!(
        f.references
            .iter()
            .find(|r| r.relation == "binds_method")
            .unwrap()
            .candidate_keys
            .is_empty()
    );
    assert!(
        f.references
            .iter()
            .find(|r| r.relation == "uses_component")
            .unwrap()
            .candidate_keys
            .contains(&"javascript:widgets:Card".to_owned())
    );
    assert_eq!(
        f.nodes
            .iter()
            .filter(|n| n.binding_key.as_deref() == Some("javascript:Page.vue:default"))
            .count(),
        1
    );
}

#[test]
fn xaml_project_conventions_toolkit_and_event_signatures() {
    use graf::languages::templates::{TemplateProject, apply_project, project_types};
    let vm_path = "Shop/ViewModels/OrdersViewModel.cs";
    let vm_source = "using CommunityToolkit.Mvvm.ComponentModel;\nusing CommunityToolkit.Mvvm.Input;\nnamespace Shop.ViewModels;\npublic partial class OrdersViewModel {\n [ObservableProperty] private string _customerName = \"\";\n [RelayCommand] private void Save() {}\n // [RelayCommand]\n private void Ignore() {}\n}\n";
    let behind_path = "Shop/Views/OrdersView.xaml.cs";
    let behind_source = "namespace Shop.Views; public partial class OrdersView { private void Ready(object sender, System.EventArgs e) {} private void Save() {} }";
    let mut types = project_types(vm_path, vm_source).unwrap();
    types.extend(project_types(behind_path, behind_source).unwrap());
    let project = TemplateProject {
        root: "Shop",
        namespace: Some("Shop"),
        types: &types,
    };
    let mut vm = facts(vm_path, vm_source);
    apply_project(&mut vm, &project);
    let generated: Vec<_> = vm
        .nodes
        .iter()
        .filter(|n| n.metadata["generated_by"].is_string())
        .collect();
    assert_eq!(generated.len(), 2);
    assert!(generated.iter().all(|n| n.file == vm_path));
    let property = generated
        .iter()
        .find(|n| n.label == "CustomerName")
        .unwrap();
    assert_eq!(property.line, Some(5));
    let property_key = property.binding_key.clone().unwrap();
    let command_key = generated
        .iter()
        .find(|n| n.label == "SaveCommand")
        .unwrap()
        .binding_key
        .clone()
        .unwrap();
    let mut behind = facts(behind_path, behind_source);
    apply_project(&mut behind, &project);
    let mut view = facts(
        "Shop/Views/OrdersView.xaml",
        "<Window xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\" x:Class=\"Shop.Views.OrdersView\"><TextBlock Text=\"{Binding CustomerName}\"/><Button Command=\"{Binding SaveCommand}\" Loaded=\"Ready\" Click=\"Save\"/></Window>",
    );
    apply_project(&mut view, &project);
    assert!(has_ref(
        &view,
        "view_model",
        "Shop.ViewModels.OrdersViewModel"
    ));
    assert_eq!(
        view.references
            .iter()
            .find(|r| r.label == "CustomerName")
            .unwrap()
            .candidate_keys,
        [property_key]
    );
    assert_eq!(
        view.references
            .iter()
            .find(|r| r.label == "SaveCommand")
            .unwrap()
            .candidate_keys,
        [command_key]
    );
    let ready = view.references.iter().find(|r| r.label == "Ready").unwrap();
    assert_eq!(ready.candidate_keys.len(), 1);
    assert!(
        behind.nodes.iter().any(
            |n| n.metadata["binding_aliases"].as_array().is_some_and(|a| a
                .iter()
                .any(|k| k.as_str() == Some(&ready.candidate_keys[0])))
        )
    );
    assert!(
        view.references
            .iter()
            .find(|r| r.label == "Save")
            .unwrap()
            .candidate_keys
            .is_empty()
    );
    let before = (vm.nodes.len(), vm.edges.len(), view.references.len());
    apply_project(&mut vm, &project);
    apply_project(&mut view, &project);
    assert_eq!(
        (vm.nodes.len(), vm.edges.len(), view.references.len()),
        before
    );
}

#[test]
fn xaml_prism_and_project_ambiguity_do_not_guess_global_names() {
    use graf::languages::templates::{TemplateProject, apply_project, project_types};
    let source = "namespace Shop.ViewModels; public class OrdersViewModel {}";
    let types = project_types("Shop/ViewModels/OrdersViewModel.cs", source).unwrap();
    let project = TemplateProject {
        root: "Shop",
        namespace: Some("Shop"),
        types: &types,
    };
    for (value, expected) in [("True", true), ("False", false)] {
        let mut f = facts(
            "Shop/Views/OrdersView.xaml",
            &format!(
                "<Window xmlns:prism=\"http://prismlibrary.com/\" prism:ViewModelLocator.AutoWireViewModel=\"{value}\"/>"
            ),
        );
        apply_project(&mut f, &project);
        assert_eq!(
            has_ref(&f, "view_model", "Shop.ViewModels.OrdersViewModel"),
            expected
        );
    }
    let mut dynamic = facts(
        "Shop/Views/OrdersView.xaml",
        "<Window xmlns:x=\"x\" x:Class=\"Shop.Views.OrdersView\" DataContext=\"{Binding RuntimeContext}\"/>",
    );
    apply_project(&mut dynamic, &project);
    assert!(
        !dynamic
            .references
            .iter()
            .any(|r| r.relation == "view_model")
    );
    let mut ambiguous_types = types.clone();
    ambiguous_types.extend(project_types("Shop/Other/OrdersViewModel.cs", source).unwrap());
    let mut view = facts(
        "Shop/Views/OrdersView.xaml",
        "<Window xmlns:x=\"x\" x:Class=\"Shop.Views.OrdersView\"/>",
    );
    apply_project(
        &mut view,
        &TemplateProject {
            root: "Shop",
            namespace: Some("Shop"),
            types: &ambiguous_types,
        },
    );
    assert!(!view.references.iter().any(|r| r.relation == "view_model"));
    let outsiders = project_types("Elsewhere/ViewModels/OrdersViewModel.cs", source).unwrap();
    apply_project(
        &mut view,
        &TemplateProject {
            root: "Shop",
            namespace: Some("Shop"),
            types: &outsiders,
        },
    );
    assert!(!view.references.iter().any(|r| r.relation == "view_model"));
    let false_attributes = "namespace Shop.ViewModels; public partial class OrdersViewModel { [Other.ObservableProperty] private string name; [Other.RelayCommand] void Save() {} }";
    assert!(
        project_types("Shop/ViewModels/OrdersViewModel.cs", false_attributes).unwrap()[0]
            .generated
            .is_empty()
    );
}

#[test]
fn robot_imports_named_libraries_and_bounded_static_variables() {
    let f = facts(
        "Tests/Sub/suite.robot",
        "*** Settings ***\nLibrary    SeleniumLibrary\nLibrary    Collections\nLibrary    ${Cur_Dir}/../Library/library.py\nResource    ${EXEC DIR}/Resource/common.robot\nResource    ${ROOT}/steps.resource\nResource    ${UNKNOWN}/missing.resource\n*** Variables ***\n${ROOT}    ../Shared\n",
    );
    assert!(
        f.nodes
            .iter()
            .any(|n| n.label == "SeleniumLibrary" && n.kind == "library")
    );
    assert!(!f.nodes.iter().any(|n| n.label == "Collections"));
    assert!(!has_ref(&f, "imports", "Collections"));
    assert!(
        f.references
            .iter()
            .any(|r| r.candidate_keys == ["module:Tests.Library.library"])
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.candidate_keys == ["template:file:Resource/common.robot"])
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.candidate_keys == ["template:file:Tests/Shared/steps.resource"])
    );
    assert!(
        f.references
            .iter()
            .find(|r| r.label == "${UNKNOWN}/missing.resource")
            .unwrap()
            .candidate_keys
            .is_empty()
    );
    let f = facts(
        "loop.robot",
        "*** Settings ***\nResource    ${ROOT}/missing.resource\n*** Variables ***\n${ROOT}    ${ROOT}\n",
    );
    assert!(f.references.iter().all(|r| r.candidate_keys.is_empty()));
}

fn official_robot(path: &str, source: &str) -> FileFacts {
    let python = std::env::var_os("GRAF_TEST_ROBOT_PYTHON")
        .expect("set GRAF_TEST_ROBOT_PYTHON to a Python environment with Robot Framework 7.5.x");
    graf::languages::templates::parse_robot_official(
        path,
        source,
        "fixture",
        std::path::Path::new(&python),
    )
    .unwrap()
}

#[test]
fn official_robot_rejects_paths_and_oversize_before_launch() {
    use graf::languages::templates::parse_robot_official;
    let absent = std::path::Path::new("missing-robot-python");
    for path in [
        "../suite.robot",
        "/suite.robot",
        "suite.py",
        "a\\suite.robot",
    ] {
        assert!(parse_robot_official(path, "", "h", absent).is_err());
    }
    let large = "x".repeat(4 * 1024 * 1024 + 1);
    let f = parse_robot_official("suite.robot", &large, "h", absent).unwrap();
    assert!(f.nodes.is_empty());
    assert!(!f.diagnostics.is_empty());
}

#[test]
#[ignore = "requires explicit GRAF_TEST_ROBOT_PYTHON with installed Robot Framework 7.5.x"]
fn official_robot_localization_and_original_source_ranges() {
    let source = "\u{feff}Language: fr\r\n*** Paramètres ***\r\nMise en place de suite    Préparer\r\n*** Cas de test ***\r\nCafé\r\n    Étant donné Préparer\r\n*** Mots-clés ***\r\nPréparer\r\n    Log    π";
    let f = official_robot("tests/suite.robot", source);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    assert_eq!(f.nodes[0].metadata["coverage"], "static-model");
    assert_eq!(f.nodes[0].metadata["parser"], "robotframework");
    let keyword = f.nodes.iter().find(|n| n.label == "Préparer").unwrap();
    assert_eq!(keyword.line, Some(8));
    let start = keyword.metadata["start_byte"].as_u64().unwrap() as usize;
    let end = keyword.metadata["end_byte"].as_u64().unwrap() as usize;
    assert_eq!(&source[start..end], "Préparer\r\n    Log    π");
    let call = f
        .references
        .iter()
        .find(|r| r.label == "Étant donné Préparer")
        .unwrap();
    assert_eq!(call.line, 6);
    assert_eq!(call.candidate_keys, [keyword.binding_key.clone().unwrap()]);
    assert!(
        f.references
            .iter()
            .any(|r| r.source == f.nodes[0].id && r.label == "Préparer")
    );
}

#[test]
#[ignore = "requires explicit GRAF_TEST_ROBOT_PYTHON with installed Robot Framework 7.5.x"]
fn official_robot_embedded_arguments_exact_priority_and_ambiguity() {
    let source = r"*** Test Cases ***
Example
    Given Greet Ada
    Greet Bob
    Value 42
    Value nope
    Pair red blue
    Do ${dynamic}
    OpenSession
    open_session
*** Keywords ***
Greet ${name}
    No Operation
Greet Bob
    No Operation
Value ${value:[0-9]+}
    No Operation
Pair ${left} blue
    No Operation
Pair red ${right}
    No Operation
Open Session
    No Operation
";
    let f = official_robot("suite.robot", source);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    for (call, name) in [
        ("Given Greet Ada", "Greet ${name}"),
        ("Greet Bob", "Greet Bob"),
        ("Value 42", "Value ${value:[0-9]+}"),
        ("OpenSession", "Open Session"),
        ("open_session", "Open Session"),
    ] {
        let key = f
            .nodes
            .iter()
            .find(|n| n.kind == "keyword" && n.label == name)
            .unwrap()
            .binding_key
            .clone()
            .unwrap();
        assert_eq!(
            f.references
                .iter()
                .find(|r| r.label == call)
                .unwrap()
                .candidate_keys,
            [key]
        );
    }
    for call in ["Value nope", "Pair red blue", "Do ${dynamic}"] {
        assert!(
            f.references
                .iter()
                .find(|r| r.label == call)
                .unwrap()
                .candidate_keys
                .is_empty()
        );
    }
}

#[test]
#[ignore = "requires explicit GRAF_TEST_ROBOT_PYTHON with installed Robot Framework 7.5.x"]
fn official_robot_control_flow_fixtures_templates_and_resource_links() {
    let source = r"*** Settings ***
Suite Setup    Prepare
Suite Teardown    Cleanup
Test Setup    Prepare
Test Teardown    Cleanup
Test Template    Check
Resource    ${Cur_Dir}${/}steps.resource
Library    SeleniumLibrary    WITH NAME    Web
Library    Collections
Variables    ${Unknown}/vars.py
*** Test Cases ***
Table
    data is not a keyword
Plain
    [Template]    NONE
    [Setup]    Prepare
    [Teardown]    Cleanup
    IF    $flag    Prepare    ELSE    Cleanup
    FOR    ${item}    IN    alpha
        Open Session
    END
    WHILE    $flag
        TRY
            Prepare
        EXCEPT
            Cleanup
        FINALLY
            Prepare
        END
        BREAK
    END
*** Keywords ***
Prepare
    [Setup]    Cleanup
    [Teardown]    Cleanup
    No Operation
Cleanup
    No Operation
Check
    [Arguments]    ${arg}
    Log    ${arg}
";
    let f = official_robot("tests/suite.robot", source);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    let resource = official_robot(
        "tests/steps.resource",
        "*** Keywords ***\nOpen Session\n    Log    ready\n",
    );
    let key = resource
        .nodes
        .iter()
        .find(|n| n.label == "Open Session")
        .unwrap()
        .binding_key
        .clone()
        .unwrap();
    let plain = f.nodes.iter().find(|n| n.label == "Plain").unwrap();
    assert!(f.references.iter().any(|r| r.source == plain.id
        && r.label == "Open Session"
        && r.candidate_keys == [key.clone()]));
    assert!(
        f.references
            .iter()
            .any(|r| r.source == f.nodes[0].id && r.label == "Prepare")
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "Check" && r.source != f.nodes[0].id)
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.label == "${Cur_Dir}${/}steps.resource"
                && r.candidate_keys == ["template:file:tests/steps.resource"])
    );
    assert!(
        f.references
            .iter()
            .find(|r| r.label == "${Unknown}/vars.py")
            .unwrap()
            .candidate_keys
            .is_empty()
    );
    assert!(
        f.nodes
            .iter()
            .any(|n| n.label == "SeleniumLibrary" && n.metadata["alias"] == "Web")
    );
    assert!(!f.nodes.iter().any(|n| n.label == "Collections"));
    for label in [
        "data is not a keyword",
        "IF",
        "FOR",
        "WHILE",
        "TRY",
        "EXCEPT",
        "BREAK",
        "alpha",
    ] {
        assert!(!has_ref(&f, "calls", label));
    }
    let f = official_robot(
        "tests/suite.robot",
        "*** Settings ***\nResource    a.resource\nResource    b.resource\n*** Test Cases ***\nCase\n    Work\n    a.Work\n",
    );
    assert!(
        f.references
            .iter()
            .find(|r| r.label == "Work")
            .unwrap()
            .candidate_keys
            .is_empty()
    );
    assert_eq!(
        f.references
            .iter()
            .find(|r| r.label == "a.Work")
            .unwrap()
            .candidate_keys,
        ["robot:keyword:tests/a.resource:work"]
    );
}

#[test]
#[ignore = "requires explicit GRAF_TEST_ROBOT_PYTHON with installed Robot Framework 7.5.x"]
fn official_robot_invalid_syntax_has_no_partial_facts() {
    for (path, source) in [
        (
            "steps.resource",
            "*** Test Cases ***\nForbidden\n    Log    text\n",
        ),
        (
            "suite.robot",
            "*** Test Cases ***\nBroken\n    IF    $flag\n        Log    text\n",
        ),
        (
            "suite.robot",
            "Language: unknown-language\n*** Test Cases ***\nCase\n    No Operation\n",
        ),
    ] {
        let f = official_robot(path, source);
        assert!(!f.diagnostics.is_empty());
        assert!(f.nodes.is_empty());
        assert!(f.references.is_empty());
    }
}

#[test]
#[ignore = "requires explicit GRAF_TEST_ROBOT_PYTHON with installed Robot Framework 7.5.x"]
fn official_robot_does_not_import_or_evaluate_user_code() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("executed");
    let payload = directory.path().join("untrusted.py");
    std::fs::write(
        &payload,
        format!(
            "from pathlib import Path\nPath({:?}).write_text('executed')\n",
            marker.to_str().unwrap()
        ),
    )
    .unwrap();
    let expression = format!(
        "${{{{__import__('pathlib').Path({:?}).write_text('executed')}}}}",
        marker.to_str().unwrap()
    );
    let source = format!(
        "*** Settings ***\nLibrary    {0}\nVariables    {0}\nResource    {0}\n*** Variables ***\n${{VALUE}}    {expression}\n*** Test Cases ***\nCase\n    Log    ${{VALUE}}\n",
        payload.display()
    );
    let f = official_robot("suite.robot", &source);
    assert!(f.diagnostics.is_empty(), "{:?}", f.diagnostics);
    assert_eq!(
        f.references
            .iter()
            .filter(|r| r.relation == "imports")
            .count(),
        3
    );
    assert!(!marker.exists());
    let f = official_robot(
        "suite.robot",
        &format!(
            "Language: {}\n*** Test Cases ***\nCase\n    No Operation\n",
            payload.display()
        ),
    );
    assert!(!f.diagnostics.is_empty());
    assert!(f.nodes.is_empty());
    assert!(!marker.exists());
}

fn store_template_project(root: &str, sources: &[(&str, &str)]) -> graf::model::GraphSnapshot {
    use graf::languages::templates::{TemplateProject, apply_project, project_types};
    let types: Vec<_> = sources
        .iter()
        .flat_map(|(path, source)| project_types(path, source).unwrap())
        .collect();
    let project = TemplateProject {
        root,
        // A project default namespace does not put global C# declarations into it.
        namespace: Some("Application"),
        types: &types,
    };
    let files = sources
        .iter()
        .map(|(path, source)| {
            let mut f = facts(path, source);
            assert!(f.diagnostics.is_empty(), "{path}: {:?}", f.diagnostics);
            apply_project(&mut f, &project);
            let once = serde_json::to_value(&f).unwrap();
            apply_project(&mut f, &project);
            assert_eq!(serde_json::to_value(&f).unwrap(), once);
            f
        })
        .collect();
    let directory = tempfile::tempdir().unwrap();
    let mut store = graf::store::Store::create(&directory.path().join("graph.db")).unwrap();
    store
        .apply_native("fixture", files, vec![], graf::model::Coverage::default())
        .unwrap();
    store.snapshot().unwrap()
}

#[test]
fn razor_injected_services_resolve_to_project_csharp_definitions_in_store() {
    let files = [
        (
            "Shop/Services/WidgetService.cs",
            "public class WidgetService {}",
        ),
        (
            "Shop/NamedService.cs",
            "namespace Demo.Services { public class NamedService {} }",
        ),
        (
            "Shop/FileService.cs",
            "namespace Demo.Services; public class FileService {}",
        ),
        (
            "Shop/First.razor",
            "@using Demo.Services\n@inject WidgetService widgets\n@inject NamedService named\n@inject FileService file\n",
        ),
        (
            "Shop/Second.cshtml",
            "@using Pick = Demo.Services.FileService\n@inject WidgetService widgets\n@inject Demo.Services.NamedService named\n@inject Pick file\n",
        ),
    ];
    for root in ["", "Shop"] {
        let graph = store_template_project(root, &files);
        for page in ["Shop/First.razor", "Shop/Second.cshtml"] {
            for (field, file, name) in [
                ("widgets", "Shop/Services/WidgetService.cs", "WidgetService"),
                ("named", "Shop/NamedService.cs", "NamedService"),
                ("file", "Shop/FileService.cs", "FileService"),
            ] {
                let source = graph
                    .nodes
                    .iter()
                    .find(|n| n.file == page && n.kind == "field" && n.label == field)
                    .unwrap();
                let target = graph
                    .nodes
                    .iter()
                    .find(|n| n.file == file && n.kind == "class" && n.label == name)
                    .unwrap();
                assert!(
                    graph.edges.iter().any(|e| e.source == source.id
                        && e.target == target.id
                        && e.relation == "uses_type"),
                    "missing {root}:{page}:{field} -> {file}:{name}"
                );
            }
        }
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| n.kind == "class" && n.label == "WidgetService")
                .count(),
            1
        );
    }
}

#[test]
fn razor_injected_services_do_not_guess_namespaces_projects_or_duplicate_types() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "@inject WidgetService service\n",
            &[(
                "Shop/Wrong.cs",
                "namespace Unrelated; public class WidgetService {}",
            )],
        ),
        (
            "@inject Missing.WidgetService service\n",
            &[("Shop/Global.cs", "public class WidgetService {}")],
        ),
        (
            "@inject WidgetService service\n",
            &[
                ("Shop/First.cs", "public class WidgetService {}"),
                ("Shop/Second.cs", "public class WidgetService {}"),
            ],
        ),
        (
            "@using One\n@using Two\n@inject WidgetService service\n",
            &[
                (
                    "Shop/First.cs",
                    "namespace One; public class WidgetService {}",
                ),
                (
                    "Shop/Second.cs",
                    "namespace Two; public class WidgetService {}",
                ),
            ],
        ),
        (
            "@inject WidgetService service\n",
            &[("Other/Global.cs", "public class WidgetService {}")],
        ),
        (
            "@inject Demo.WidgetService service\n",
            &[(
                "Other/Named.cs",
                "namespace Demo; public class WidgetService {}",
            )],
        ),
        (
            "@inject WidgetService service\n",
            &[(
                "Shop/Hidden.cs",
                "public class Container { private class WidgetService {} }",
            )],
        ),
    ];
    for (page, definitions) in cases {
        let mut files = definitions.to_vec();
        files.push(("Shop/Page.razor", page));
        let graph = store_template_project("Shop", &files);
        let field = graph
            .nodes
            .iter()
            .find(|n| n.file == "Shop/Page.razor" && n.kind == "field" && n.label == "service")
            .unwrap();
        assert!(
            !graph
                .edges
                .iter()
                .any(|e| e.source == field.id && e.relation == "uses_type"),
            "unexpected binding for {page}: {:?}",
            definitions
        );
    }
}
