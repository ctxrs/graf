use graf::ingest::{self, CommandAdapter, IngestOptions, Provider, SemanticOptions};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    thread,
};

fn options() -> IngestOptions {
    IngestOptions::default()
}

#[test]
fn documents_extract_evidence_without_following_links() {
    let markdown = "---\ntitle: Design\n---\n# Engine\nWe chose queues because retries matter.\n[Overview](../README.md) and `Engine`.\n[Remote](http://127.0.0.1:1/never-fetch)\n```md\n# Fake heading\n[hidden](secret.md)\n```\n";
    let facts = ingest::extract_text("docs/design.mdx", markdown, "h", &options()).unwrap();
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "heading" && n.label == "Engine" && n.line == Some(4))
    );
    assert!(!facts.nodes.iter().any(|n| n.label == "Fake heading"));
    assert!(facts.nodes.iter().any(|n| n.kind == "rationale"));
    assert!(
        facts
            .references
            .iter()
            .any(|r| r.candidate_keys == ["file:README.md"])
    );
    assert!(
        facts
            .references
            .iter()
            .any(|r| r.candidate_keys == ["symbol:Engine"] && r.relation == "documents")
    );
    assert!(!facts.references.iter().any(|r| r.label == "hidden"));
    assert_eq!(facts.nodes[0].metadata["frontmatter"]["title"], "Design");
    assert!(ingest::extract_text("../bad.md", "x", "h", &options()).is_err());
    assert!(ingest::extract_text("/bad.md", "x", "h", &options()).is_err());
}

#[test]
fn malformed_url_examples_do_not_abort_documents_or_become_link_evidence() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let valid = format!("http://{}/never-fetch", listener.local_addr().unwrap());
    for (path, text) in [
        (
            "notes.txt",
            format!(
                "Example http://localhost:PORT/path\n{valid}\nhttps://user:SYNTHETIC_SECRET@example.invalid/"
            ),
        ),
        (
            "notes.md",
            format!(
                "# Valid document\nExample http://localhost:PORT/path\n[Good]({valid}) [Bad](http://localhost:PORT/path) [Sensitive](https://user:SYNTHETIC_SECRET@example.invalid/)"
            ),
        ),
        (
            "notes.html",
            format!(
                "<h1>Valid document</h1><p>Example http://localhost:PORT/path</p><a href='{valid}'>Good</a><a href='http://localhost:PORT/path'>Bad</a><a href='https://user:SYNTHETIC_SECRET@example.invalid/'>Sensitive</a>"
            ),
        ),
    ] {
        let facts = ingest::extract_text(path, &text, "h", &options()).unwrap();
        assert_eq!(facts.path, path);
        assert!(!facts.nodes.is_empty());
        let links: Vec<_> = facts.nodes.iter().filter(|n| n.kind == "url").collect();
        assert!(!links.is_empty());
        assert!(links.iter().all(|n| n.metadata["url"] == valid));
        assert!(
            !serde_json::to_string(&facts.edges)
                .unwrap()
                .contains("SYNTHETIC_SECRET")
        );
        assert!(facts.diagnostics.is_empty());
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    for url in [
        "http://localhost:PORT/path",
        "https://user:SYNTHETIC_SECRET@example.invalid/",
    ] {
        let error = ingest::extract_url(url, "notes.txt", &options()).unwrap_err();
        assert!(!format!("{error:?}").contains("SYNTHETIC_SECRET"));
    }
}

#[test]
fn html_rst_yaml_and_pointer_inputs() {
    let html = ingest::extract_text(
        "page.html",
        "<h1>Title &amp; More</h1><a href='readme.md'>Read</a><script><h1>Hidden</h1></script>",
        "h",
        &options(),
    )
    .unwrap();
    assert!(
        html.nodes
            .iter()
            .any(|n| n.label == "Title & More" && n.line.is_none())
    );
    assert!(
        html.references
            .iter()
            .any(|r| r.candidate_keys == ["file:readme.md"])
    );
    let rst = ingest::extract_text(
        "page.rst",
        "Decision\n========\nRationale: retries. See `guide <guide.md>`_.",
        "h",
        &options(),
    )
    .unwrap();
    assert!(rst.nodes.iter().any(|n| n.kind == "heading"));
    assert!(
        rst.references
            .iter()
            .any(|r| r.candidate_keys == ["file:guide.md"])
    );
    let yaml = ingest::extract_text(
        "page.yaml",
        "name: Queue\nreason: 'because durability matters'\ndocs: https://example.invalid/spec",
        "h",
        &options(),
    )
    .unwrap();
    assert!(
        yaml.nodes
            .iter()
            .any(|n| n.kind == "document_field" && n.label == "name")
    );
    assert!(yaml.nodes.iter().any(|n| n.kind == "rationale"));
    assert!(ingest::extract_text("bad.yaml", "[a: [broken", "h", &options()).is_err());
    let pointer=ingest::extract_text("design.gdoc",r#"{"url":"https://docs.google.com/document/d/abc_123/edit","email":"private@example.invalid"}"#,"h",&options()).unwrap();
    assert_eq!(pointer.nodes[0].metadata["pointer"]["file_id"], "abc_123");
    assert!(
        !serde_json::to_string(&pointer.nodes)
            .unwrap()
            .contains("private@example.invalid")
    );
    let url = ingest::extract_text(
        "site.url",
        "[InternetShortcut]\nURL=http://127.0.0.1:1/unreachable",
        "h",
        &options(),
    )
    .unwrap();
    assert!(url.nodes.iter().any(|n| n.kind == "url"));
}

fn zip_file(path: &Path, members: &[(&str, &str)]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (name, content) in members {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn office_native_zip_xml_and_malformed_archives() {
    let temp = tempfile::tempdir().unwrap();
    let docx = temp.path().join("spec.docx");
    zip_file(
        &docx,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="urn:w"><w:body><w:p><w:r><w:t>Decision: because durability matters.</w:t></w:r></w:p></w:body></w:document>"#,
        )],
    );
    let facts = ingest::extract(&docx, "spec.docx", "h", &options()).unwrap();
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "rationale" && n.label.contains("durability"))
    );
    let xlsx = temp.path().join("sheet.xlsx");
    zip_file(
        &xlsx,
        &[
            (
                "xl/sharedStrings.xml",
                "<sst><si><t>Decision: queue</t></si><si><t>because durability</t></si></sst>",
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c t="s"><v>0</v></c><c t="inlineStr"><is><t>first</t></is></c></row></sheetData></worksheet>"#,
            ),
            (
                "xl/worksheets/sheet2.xml",
                r#"<worksheet><sheetData><row><c t="s"><v>1</v></c><c><f>DO_NOT_EXECUTE()</f><v>42</v></c></row></sheetData></worksheet>"#,
            ),
        ],
    );
    let facts = ingest::extract(&xlsx, "sheet.xlsx", "h", &options()).unwrap();
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.label.contains("Decision: queue"))
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.label.contains("because durability"))
    );
    zip_file(
        &docx,
        &[("../escape", "bad"), ("word/document.xml", "<document/>")],
    );
    assert!(ingest::extract(&docx, "spec.docx", "h", &options()).is_err());
    zip_file(
        &docx,
        &[(
            "word/document.xml",
            "<!DOCTYPE d [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><d>&x;</d>",
        )],
    );
    assert!(ingest::extract(&docx, "spec.docx", "h", &options()).is_err());
}

fn pdf_bytes() -> Vec<u8> {
    let stream = "BT /F1 12 Tf 72 720 Td (Decision: because durability matters.) Tj ET";
    let objects=["<< /Type /Catalog /Pages 2 0 R >>".into(),"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),format!("<< /Length {} >>\nstream\n{stream}\nendstream",stream.len())];
    pdf_objects(&objects)
}

// Assemble fixtures independently of the PDF library under test.
fn pdf_objects(objects: &[String]) -> Vec<u8> {
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = vec![0];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
    }
    let xref = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()));
    for offset in offsets.iter().skip(1) {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF",
        offsets.len()
    ));
    pdf.into_bytes()
}

#[test]
fn pdf_native_extraction_and_binary_limits() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sample.pdf");
    std::fs::write(&path, pdf_bytes()).unwrap();
    let facts = ingest::extract(&path, "sample.pdf", "h", &options()).unwrap();
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "rationale" && n.label.contains("durability"))
    );
    let mut small = options();
    small.max_input_bytes = 4;
    assert!(ingest::extract(&path, "sample.pdf", "h", &small).is_err());
    std::fs::write(&path, "not a pdf").unwrap();
    assert!(ingest::extract(&path, "sample.pdf", "h", &options()).is_err());
}

fn pdf_stream(content: &str) -> String {
    format!(
        "<< /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    )
}

#[test]
fn pdf_page_tree_order_and_to_unicode_text() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("pages.pdf");
    // Page object IDs deliberately disagree with reading order.
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n        /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n        /CMapName /Fixture def /CMapType 2 def\n        1 begincodespacerange <00> <FF> endcodespacerange\n        3 beginbfchar <01> <03A9> <02> <4E2D> <03> <00E9> endbfchar\n        endcmap CMapName currentdict /CMap defineresource pop end end";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [6 0 R 3 0 R] /Count 2 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        pdf_stream("BT /F1 12 Tf (Second page) Tj ET"),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R /F2 8 0 R >> >> /Contents 7 0 R >>".into(),
        pdf_stream("BT /F1 12 Tf (First page) Tj ET BT /F2 12 Tf <010203> Tj ET"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol /Encoding << /Type /Encoding /BaseEncoding 42 /Differences [] >> /ToUnicode 9 0 R >>".into(),
        pdf_stream(cmap),
    ];
    std::fs::write(&path, pdf_objects(&objects)).unwrap();
    let facts = ingest::extract(&path, "pages.pdf", "h", &options()).unwrap();
    assert_eq!(
        facts.nodes[0].metadata["text"],
        "First page\nΩ中é\nSecond page\n"
    );
    assert_eq!(facts.nodes[0].metadata["converter"], "lopdf");
    let mut small = options();
    small.max_text_bytes = 8;
    assert!(
        ingest::extract(&path, "pages.pdf", "h", &small)
            .unwrap_err()
            .to_string()
            .contains("text byte limit")
    );
}

fn pdf_content_fixture(content: &str, resources: &str, extra: Vec<String>) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources {resources} /Contents 5 0 R >>"
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        pdf_stream(content),
    ];
    objects.extend(extra);
    pdf_objects(&objects)
}

fn pdf_form(content: &str, entries: &str) -> String {
    format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 612 792] {entries} /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    )
}

#[test]
fn pdf_forms_keep_text_local_resources_and_caller_graphics() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("forms.pdf");
    for page in [
        "/Outer Do",
        "BT /F1 12 Tf <80> Tj ET /Outer Do BT <80> Tj ET q BT /F2 12 Tf <80> Tj ET Q BT <80> Tj ET",
    ] {
        let bytes = pdf_content_fixture(page,
            "<< /Font << /F1 4 0 R /F2 7 0 R >> /XObject << /Outer 6 0 R >> >>",
            vec![
                pdf_form("BT /F1 12 Tf (FormMarker) Tj <80> Tj ET /Nested Do",
                    "/Resources << /Font << /F1 7 0 R >> /XObject << /Nested 8 0 R >> >>"),
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /MacRomanEncoding >>".into(),
                pdf_form("BT <80> Tj ET", ""),
            ]);
        std::fs::write(&path, bytes).unwrap();
        let facts = ingest::extract(&path, "forms.pdf", "h", &options()).unwrap();
        let expected = if page == "/Outer Do" {
            "FormMarkerÄ\nÄ\n"
        } else {
            "€\nFormMarkerÄ\nÄ\n€\nÄ\n€\n"
        };
        assert_eq!(facts.nodes[0].metadata["text"], expected);
    }
}

#[test]
fn pdf_positioned_lines_preserve_word_boundaries_and_same_baseline_fragments() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("position.pdf");
    for (movement, expected) in [
        ("0 -24 Td", "Alpha\nBeta\n"),
        ("0 -24 TD", "Alpha\nBeta\n"),
        ("1 0 0 1 72 696 Tm", "Alpha\nBeta\n"),
        ("30 0 Td", "AlphaBeta\n"),
        ("30 0 TD", "AlphaBeta\n"),
        ("1 0 0 1 102 720 Tm", "AlphaBeta\n"),
        ("", "AlphaBeta\n"),
        ("24 TL T*", "Alpha\nBeta\n"),
    ] {
        let content = format!("BT /F1 12 Tf 1 0 0 1 72 720 Tm (Alpha) Tj {movement} (Beta) Tj ET");
        std::fs::write(
            &path,
            pdf_content_fixture(&content, "<< /Font << /F1 4 0 R >> >>", vec![]),
        )
        .unwrap();
        let facts = ingest::extract(&path, "position.pdf", "h", &options()).unwrap();
        assert_eq!(facts.nodes[0].metadata["text"], expected, "{movement}");
    }
}

#[test]
fn pdf_repeated_forms_are_allowed_but_cycles_depth_and_work_are_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bounded.pdf");
    let resources = "<< /Font << /F1 4 0 R >> /XObject << /Form 6 0 R >> >>";
    let ordinary = pdf_form("BT /F1 12 Tf (Repeat) Tj ET", "");
    std::fs::write(
        &path,
        pdf_content_fixture("/Form Do /Form Do", resources, vec![ordinary.clone()]),
    )
    .unwrap();
    let facts = ingest::extract(&path, "bounded.pdf", "h", &options()).unwrap();
    assert_eq!(facts.nodes[0].metadata["text"], "Repeat\nRepeat\n");
    std::fs::write(
        &path,
        pdf_content_fixture("/Form Do", resources, vec![pdf_form("/Form Do", "")]),
    )
    .unwrap();
    assert!(
        ingest::extract(&path, "bounded.pdf", "h", &options())
            .unwrap_err()
            .to_string()
            .contains("Form cycle")
    );
    for depth in [3, 70] {
        let forms = (0..depth)
            .map(|i| {
                if i + 1 == depth {
                    ordinary.clone()
                } else {
                    pdf_form(
                        "/Form Do",
                        &format!(
                            "/Resources << /Font << /F1 4 0 R >> /XObject << /Form {} 0 R >> >>",
                            7 + i
                        ),
                    )
                }
            })
            .collect();
        std::fs::write(&path, pdf_content_fixture("/Form Do", resources, forms)).unwrap();
        let result = ingest::extract(&path, "bounded.pdf", "h", &options());
        if depth == 3 {
            assert_eq!(result.unwrap().nodes[0].metadata["text"], "Repeat\n");
        } else {
            assert!(result.unwrap_err().to_string().contains("nesting limit"));
        }
    }
    // The file fits: repeated expansion, not input size, exhausts the budget.
    let repeated = pdf_content_fixture(
        &"/Form Do ".repeat(100),
        resources,
        vec![pdf_form(
            &format!("%{}\nBT /F1 12 Tf (Repeat) Tj ET", "padding".repeat(80)),
            "",
        )],
    );
    let mut limited = options();
    limited.max_input_bytes = repeated.len() as u64;
    std::fs::write(&path, repeated).unwrap();
    assert!(ingest::extract(&path, "bounded.pdf", "h", &limited).is_err());
}

#[test]
fn pdf_shared_page_content_stops_at_text_budget_before_later_errors() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("shared.pdf");
    let page = |content| {
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents {content} 0 R >>"
        )
    };
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".into(),
        page(5),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        pdf_stream("BT /F1 12 Tf (Shared) Tj ET"),
        page(5),
        page(8),
        pdf_stream("BT /Missing 12 Tf (Unreachable with small budget) Tj ET"),
    ];
    std::fs::write(&path, pdf_objects(&objects)).unwrap();
    assert_eq!(
        ingest::extract(&path, "shared.pdf", "h", &options())
            .unwrap()
            .nodes[0]
            .metadata["text"],
        "Shared\nShared\n"
    );
    objects[1] = "<< /Type /Pages /Kids [3 0 R 6 0 R 7 0 R] /Count 3 >>".into();
    std::fs::write(&path, pdf_objects(&objects)).unwrap();
    let mut small = options();
    small.max_text_bytes = 10;
    assert!(
        ingest::extract(&path, "shared.pdf", "h", &small)
            .unwrap_err()
            .to_string()
            .contains("text byte limit")
    );
    assert!(
        !ingest::extract(&path, "shared.pdf", "h", &options())
            .unwrap_err()
            .to_string()
            .contains("text byte limit")
    );
}

#[test]
fn pdf_graphics_transforms_restore_after_q_and_form_calls() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("transforms.pdf");
    let content =
        "BT /F1 12 Tf (Alpha) Tj q 1 0 0 1 0 -20 cm (Beta) Tj Q (Gamma) Tj /Form Do (Delta) Tj ET";
    std::fs::write(
        &path,
        pdf_content_fixture(
            content,
            "<< /Font << /F1 4 0 R >> /XObject << /Form 6 0 R >> >>",
            vec![pdf_form("(Form) Tj", "/Matrix [1 0 0 1 0 -40]")],
        ),
    )
    .unwrap();
    let facts = ingest::extract(&path, "transforms.pdf", "h", &options()).unwrap();
    assert_eq!(
        facts.nodes[0].metadata["text"],
        "Alpha\nBeta\nGamma\nForm\nDelta\n"
    );
}

#[test]
fn pdf_compressed_content_and_unicode_maps_obey_decoding_budget() {
    // Independently zlib-compressed fixtures: 8192 spaces followed by page text
    // or a CMap mapping byte 01 to U+03A9. ASCIIHex keeps the fixture printable.
    let compressed = [
        "789cedc1310d80401405302b6f84891c122e01055fc2c1c00013fed141d2360100000000000000feae5796bda5ada933537fde7b1c634e5dd9ea034c6e07bf>",
        "789cedd0c16ac4201006e0fb3ec51cb7a724bba742082c5b0239745b9af6018c4eb242a3620c346f5fb5db853e42e1ff40c171464789000000000000000000000000000000e0bf2bcedd536774a0e2d55bd973a0511be579b1ab974c034fda507520a565b8adf22c67e176a9b8df96c07367464b754dc55bdc5c82df687f5276e0072a5ebc62afcd44fb8f731fd7fdeadc27cf6c0295d434a4788c073d0b77113353d1eaafb07a4e61cae1f7cd311d725a75bbdb2a5e9c90ec859998eab26ca86edb86d8a8bf7bbf15c328afc2c7cc2a6696c7d363cefd89ee52557c0ddd7b90abf7b1bdfce4dc42ba5c1bbeff8ab32ed5a7f10d15e56bd3>",
    ];
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("compressed.pdf");
    for (index, hex) in compressed.iter().enumerate() {
        let filtered = format!(
            "<< /Length {} /Filter [/ASCIIHexDecode /FlateDecode] >>\nstream\n{hex}\nendstream",
            hex.len()
        );
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            filtered.clone(),
        ];
        if index == 1 {
            objects[3] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /ToUnicode 6 0 R >>".into();
            objects[4] = pdf_stream("BT /F1 12 Tf <01> Tj ET");
            objects.push(filtered);
        }
        let bytes = pdf_objects(&objects);
        assert!(bytes.len() < 2048);
        std::fs::write(&path, bytes).unwrap();
        let mut bounded = options();
        bounded.max_input_bytes = 16384;
        let facts = ingest::extract(&path, "compressed.pdf", "h", &bounded).unwrap();
        assert_eq!(
            facts.nodes[0].metadata["text"],
            if index == 0 { "Bounded\n" } else { "Ω\n" }
        );
        bounded.max_input_bytes = 2048;
        assert!(ingest::extract(&path, "compressed.pdf", "h", &bounded).is_err());
    }
}

#[test]
fn pdf_missing_or_unsupported_fonts_fail_instead_of_dropping_text() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("font.pdf");
    for resources in [
        "<< /Font << >> >>",
        "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Symbol >> >> >>",
        "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats >> >> >>",
        "<< /Font << /F1 << /Type /Font /Subtype /Type0 /BaseFont /Custom /Encoding /Identity-H >> >> >>",
        "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Custom /ToUnicode 6 0 R >> >> >>",
        "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Custom /Encoding << /Type /Encoding /Differences [0 /UnrecognizedSyntheticGlyph] >> >> >> >>",
    ] {
        std::fs::write(
            &path,
            pdf_content_fixture(
                "BT /F1 12 Tf (Do not drop me) Tj ET",
                resources,
                vec![pdf_stream("invalid cmap")],
            ),
        )
        .unwrap();
        assert!(ingest::extract(&path, "font.pdf", "h", &options()).is_err());
    }
    std::fs::write(
        &path,
        pdf_content_fixture(
            "BT /F1 12 Tf (Supported text) Tj ET",
            "<< /Font << /F1 4 0 R >> >>",
            vec![],
        ),
    )
    .unwrap();
    assert_eq!(
        ingest::extract(&path, "font.pdf", "h", &options())
            .unwrap()
            .nodes[0]
            .metadata["text"],
        "Supported text\n"
    );
}

#[test]
fn pdf_difference_encodings_require_a_supported_base() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("differences.pdf");
    for font in ["Symbol", "ZapfDingbats", "Custom"] {
        for differences in ["[]", "[90 /A]"] {
            let resources = format!(
                "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /{font} /Encoding << /Type /Encoding /Differences {differences} >> >> >> >>"
            );
            std::fs::write(
                &path,
                pdf_content_fixture("BT /F1 12 Tf (AB) Tj ET", &resources, vec![]),
            )
            .unwrap();
            assert!(ingest::extract(&path, "differences.pdf", "h", &options()).is_err());
        }
    }
    for (font, base, expected) in [
        ("Helvetica", "", Some("AZ\n")),
        ("Symbol", "/BaseEncoding /WinAnsiEncoding", Some("AZ\n")),
        ("Helvetica", "/BaseEncoding 42", None),
        ("Helvetica", "/BaseEncoding /UnknownEncoding", None),
    ] {
        let resources = format!(
            "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /{font} /Encoding << /Type /Encoding {base} /Differences [66 /Z] >> >> >> >>"
        );
        std::fs::write(
            &path,
            pdf_content_fixture("BT /F1 12 Tf (AB) Tj ET", &resources, vec![]),
        )
        .unwrap();
        let result = ingest::extract(&path, "differences.pdf", "h", &options());
        if let Some(text) = expected {
            assert_eq!(result.unwrap().nodes[0].metadata["text"], text);
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn pdf_malformed_and_deep_nesting_are_bounded() {
    const CHILD: &str = "GRAF_PDF_NESTING_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested.pdf");
        std::fs::write(&path, b"%PDF-1.4\ntruncated").unwrap();
        assert!(ingest::extract(&path, "nested.pdf", "h", &options()).is_err());
        for depth in [1, 10_000] {
            let stream = format!(
                "BT /F1 12 Tf {}(Nested text){} TJ ET",
                "[".repeat(depth),
                "]".repeat(depth)
            );
            let objects = vec![
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
                pdf_stream(&stream),
            ];
            std::fs::write(&path, pdf_objects(&objects)).unwrap();
            let result = ingest::extract(&path, "nested.pdf", "h", &options());
            if depth == 1 {
                assert_eq!(
                    result.unwrap().nodes[0].metadata["text"]
                        .as_str()
                        .unwrap()
                        .trim(),
                    "Nested text"
                );
            } else {
                assert!(
                    result.is_err(),
                    "malformed content must not publish empty facts"
                );
            }
        }
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "pdf_malformed_and_deep_nesting_are_bounded",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "PDF regression subprocess failed: {status}"
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("PDF regression subprocess exceeded ten seconds");
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn mock(response: Value) -> (String, thread::JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/endpoint", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut bytes = vec![];
        let mut buffer = [0; 4096];
        let header_end = loop {
            let n = stream.read(&mut buffer).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|s| s.trim().parse().unwrap())
            })
            .unwrap_or(0);
        while bytes.len() < header_end + length {
            let n = stream.read(&mut buffer).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buffer[..n]);
        }
        let request = if length > 0 {
            serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
        } else {
            Value::Null
        };
        let body = response.to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        request
    });
    (url, handle)
}

fn graph() -> Value {
    json!({"nodes":[{"id":"a","label":"Queue","kind":"concept","evidence":"Queue"},{"id":"b","label":"Durability","kind":"concept","evidence":"durability"}],"edges":[{"source":"a","target":"b","relation":"supports","confidence":0.85,"evidence":"Queue provides durability"}]})
}
fn semantic(provider: Provider, endpoint: String) -> IngestOptions {
    let mut o = options();
    o.semantic = Some(SemanticOptions {
        provider,
        endpoint,
        ..SemanticOptions::default()
    });
    o
}

#[test]
fn provider_wire_formats_and_inferred_provenance() {
    let text = "Queue provides durability";
    for provider in [
        Provider::OpenAi,
        Provider::Azure,
        Provider::Anthropic,
        Provider::Gemini,
        Provider::Ollama,
    ] {
        let content = graph().to_string();
        let response = match provider {
            Provider::OpenAi | Provider::Azure => {
                json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]})
            }
            Provider::Anthropic => {
                json!({"stop_reason":"end_turn","content":[{"type":"text","text":content}]})
            }
            Provider::Gemini => {
                json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":content}]}}]})
            }
            Provider::Ollama => {
                json!({"done":true,"done_reason":"stop","message":{"content":content}})
            }
            _ => unreachable!(),
        };
        let (endpoint, server) = mock(response);
        let o = semantic(provider, endpoint);
        let facts = ingest::extract_text("doc.txt", text, "h", &o).unwrap();
        let request = server.join().unwrap();
        assert!(facts.edges.iter().any(|e| e.relation == "supports"
            && e.confidence == "inferred"
            && e.metadata["confidence_score"] == 0.85));
        assert!(facts.nodes.iter().any(
            |n| n.metadata["model"] == "gpt-6-astra" && n.metadata["provenance"] == "semantic"
        ));
        match provider {
            Provider::OpenAi | Provider::Azure => {
                assert_eq!(request["max_completion_tokens"], 2048);
                assert_eq!(request["messages"][0]["role"], "system");
            }
            Provider::Anthropic => {
                assert_eq!(request["max_tokens"], 2048);
                assert!(request["system"].is_string());
            }
            Provider::Gemini => {
                assert_eq!(request["generationConfig"]["maxOutputTokens"], 2048);
                assert!(request["contents"][0]["parts"][0]["text"].is_string());
            }
            Provider::Ollama => {
                assert_eq!(request["options"]["num_predict"], 2048);
                assert_eq!(request["stream"], false);
            }
            _ => {}
        }
    }
}

#[test]
fn invalid_semantic_responses_fail_and_successful_batches_cache() {
    for response in [
        json!({"choices":[{"finish_reason":"length","message":{"content":graph().to_string()}}]}),
        json!({"choices":[{"finish_reason":"stop","message":{"content":"{broken"}}]}),
        json!({"choices":[{"finish_reason":"stop","message":{"content":"{\"nodes\":[],\"edges\":[{\"source\":\"missing\"}]}"}}]}),
    ] {
        let (endpoint, server) = mock(response);
        assert!(
            ingest::extract_text(
                "doc.txt",
                "Queue provides durability",
                "h",
                &semantic(Provider::OpenAi, endpoint)
            )
            .is_err()
        );
        server.join().unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let (endpoint, server) = mock(
        json!({"choices":[{"finish_reason":"stop","message":{"content":graph().to_string()}}]}),
    );
    let mut o = semantic(Provider::OpenAi, endpoint);
    o.semantic.as_mut().unwrap().cache_dir = Some(temp.path().into());
    let first = ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
    server.join().unwrap();
    let second = ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
    assert_eq!(first.nodes.len(), second.nodes.len());
    let before = ingest::config_fingerprint(&o).unwrap();
    o.semantic.as_mut().unwrap().model = "explicit-other-model".into();
    assert_ne!(before, ingest::config_fingerprint(&o).unwrap());
    assert!(ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).is_err());
    let mut o = options();
    o.semantic = Some(SemanticOptions::default());
    assert!(ingest::config_fingerprint(&o).is_err());
}

#[test]
fn input_budget_never_silently_truncates_or_calls_provider() {
    let mut o = semantic(Provider::OpenAi, "http://127.0.0.1:1/no-call".into());
    let s = o.semantic.as_mut().unwrap();
    s.max_calls = 1;
    s.max_input_tokens = 2048;
    let error = ingest::extract_text("doc.txt", &"x".repeat(2048), "h", &o)
        .unwrap_err()
        .to_string();
    assert!(error.contains("call budget"));
    o.semantic.as_mut().unwrap().endpoint = "https://user:secret@example.invalid/v1".into();
    assert!(ingest::config_fingerprint(&o).is_err());
    o.semantic.as_mut().unwrap().endpoint = "https://example.invalid/v1?api_key=secret".into();
    assert!(ingest::config_fingerprint(&o).is_err());
}

#[cfg(unix)]
fn python_adapter(temp: &Path, body: &str) -> CommandAdapter {
    let script = temp.join("adapter.py");
    std::fs::write(&script, body).unwrap();
    CommandAdapter {
        program: "python3".into(),
        args: vec![
            script.to_string_lossy().into(),
            "{input}".into(),
            "{output}".into(),
        ],
        output_file: false,
    }
}

#[cfg(unix)]
#[test]
fn explicit_media_converter_argv_and_google_no_implicit_network() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("hostile ; $(touch DO_NOT_CREATE).png");
    std::fs::write(&path, b"synthetic media").unwrap();
    let default = ingest::extract(&path, "image.png", "h", &options()).unwrap();
    assert_eq!(default.nodes[0].kind, "media");
    assert_eq!(default.diagnostics.len(), 1);
    let adapter = python_adapter(
        temp.path(),
        "import pathlib,sys\nassert pathlib.Path(sys.argv[1]).read_bytes()==b'synthetic media'\nprint('Decision: because OCR evidence matters')\n",
    );
    let mut o = options();
    o.converters.insert("png".into(), adapter.clone());
    let facts = ingest::extract(&path, "image.png", "h", &o).unwrap();
    assert!(facts.nodes.iter().any(|n| n.kind == "rationale"));
    assert!(!temp.path().join("DO_NOT_CREATE").exists());
    for ext in ["mp3", "wav", "mp4", "webm"] {
        assert!(ingest::supports(Path::new(&format!("file.{ext}"))));
    }
    let google = temp.path().join("design.gdoc");
    std::fs::write(&google, r#"{"doc_id":"abc_123"}"#).unwrap();
    o.converters.insert(
        "gdoc".into(),
        CommandAdapter {
            program: "does-not-exist-never-run".into(),
            ..Default::default()
        },
    );
    assert!(ingest::extract(&google, "design.gdoc", "h", &o).is_ok());
    assert!(ingest::extract_google(&google, "design.gdoc", &o).is_err());
    let adapter = python_adapter(
        temp.path(),
        "import pathlib,sys,json\nassert json.loads(pathlib.Path(sys.argv[1]).read_text())['doc_id']=='abc_123'\npathlib.Path(sys.argv[2]).write_text('Decision: because Google export is explicit')\n",
    );
    o.converters.insert(
        "gdoc".into(),
        CommandAdapter {
            output_file: true,
            ..adapter
        },
    );
    let facts = ingest::extract_google(&google, "design.gdoc", &o).unwrap();
    assert!(facts.nodes.iter().any(|n| n.kind == "rationale"));
    assert_eq!(
        CommandAdapter::tesseract().args,
        ["{input}", "{output_stem}"]
    );
    assert!(
        CommandAdapter::whisper("base")
            .args
            .contains(&"{output_dir}".into())
    );
}

#[cfg(unix)]
#[test]
fn converter_timeout_output_limit_and_cli_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("media.wav");
    std::fs::write(&path, b"media").unwrap();
    let mut o = options();
    o.timeout_secs = 1;
    o.converters.insert(
        "wav".into(),
        python_adapter(temp.path(), "import time\ntime.sleep(5)\n"),
    );
    let start = std::time::Instant::now();
    assert!(
        ingest::extract(&path, "media.wav", "h", &o)
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    assert!(start.elapsed().as_secs() < 4);
    o.max_text_bytes = 10;
    o.converters.insert(
        "wav".into(),
        python_adapter(temp.path(), "print('x'*10000)\n"),
    );
    assert!(ingest::extract(&path, "media.wav", "h", &o).is_err());
    let body = format!(
        "import sys,json\np=json.load(sys.stdin)\nassert p['model']=='gpt-6-astra'\nassert p['max_output_tokens']==2048\nprint({:?})\n",
        graph().to_string()
    );
    let mut o = options();
    o.semantic = Some(SemanticOptions {
        provider: Provider::Cli,
        command: Some(python_adapter(temp.path(), &body)),
        ..Default::default()
    });
    let facts = ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
    assert!(facts.edges.iter().any(|e| e.relation == "supports"));
}

#[test]
fn explicit_url_read_and_content_configuration_fingerprints() {
    let (url, server) = mock(json!({"message":"Decision: because explicit retrieval"}));
    let mut o = options();
    o.allow_private_urls = true;
    let facts = ingest::extract_url(&url, "download.txt", &o).unwrap();
    server.join().unwrap();
    assert_eq!(facts.nodes[0].metadata["source_url"], url);
    assert_ne!(
        ingest::content_fingerprint(b"a", &options()).unwrap(),
        ingest::content_fingerprint(b"b", &options()).unwrap()
    );
    let mut changed = options();
    changed
        .converters
        .insert("png".into(), CommandAdapter::tesseract());
    assert_ne!(
        ingest::config_fingerprint(&options()).unwrap(),
        ingest::config_fingerprint(&changed).unwrap()
    );
}

#[test]
fn vision_payloads_are_explicit_and_provenance_is_not_literal_text() {
    use base64::Engine;
    let temp = tempfile::tempdir().unwrap();
    let image = temp.path().join("diagram.png");
    let pixels=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=").unwrap();
    std::fs::write(&image, &pixels).unwrap();
    for provider in [
        Provider::OpenAi,
        Provider::Azure,
        Provider::Anthropic,
        Provider::Gemini,
        Provider::Ollama,
    ] {
        let content = graph().to_string();
        let response = match provider {
            Provider::OpenAi | Provider::Azure => {
                json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]})
            }
            Provider::Anthropic => {
                json!({"stop_reason":"end_turn","content":[{"type":"text","text":content}]})
            }
            Provider::Gemini => {
                json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":content}]}}]})
            }
            Provider::Ollama => {
                json!({"done":true,"done_reason":"stop","message":{"content":content}})
            }
            _ => unreachable!(),
        };
        let (url, server) = mock(response);
        let mut o = semantic(provider, url);
        // Configured text semantics alone never uploads pixels.
        assert_eq!(
            ingest::extract(&image, "diagram.png", "h", &o)
                .unwrap()
                .nodes
                .len(),
            1
        );
        o.semantic.as_mut().unwrap().vision = true;
        let facts = ingest::extract(&image, "diagram.png", "h", &o).unwrap();
        assert!(
            facts
                .nodes
                .iter()
                .any(|n| n.metadata["provenance"] == "visual_inference" && n.line.is_none())
        );
        let request = server.join().unwrap();
        let encoded = match provider {
            Provider::OpenAi | Provider::Azure => request["messages"][1]["content"][1]["image_url"]
                ["url"]
                .as_str()
                .unwrap()
                .strip_prefix("data:image/png;base64,")
                .unwrap(),
            Provider::Anthropic => request["messages"][0]["content"][1]["source"]["data"]
                .as_str()
                .unwrap(),
            Provider::Gemini => request["contents"][0]["parts"][1]["inlineData"]["data"]
                .as_str()
                .unwrap(),
            Provider::Ollama => request["messages"][1]["images"][0].as_str().unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            pixels
        );
    }
}

#[cfg(unix)]
#[test]
fn installed_tool_recipes_have_real_argv_and_output_contracts() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("scan.png");
    std::fs::write(&input, b"synthetic").unwrap();
    let mut o = options();
    let script = temp.path().join("tool.py");
    std::fs::write(&script,"import pathlib,sys\nassert len(sys.argv)==3\nassert pathlib.Path(sys.argv[1]).read_bytes()==b'synthetic'\npathlib.Path(sys.argv[2]+'.txt').write_text('Decision: because OCR')\n").unwrap();
    let mut recipe = CommandAdapter::tesseract();
    recipe.program = "python3".into();
    recipe.args.insert(0, script.to_string_lossy().into());
    o.converters.insert("png".into(), recipe);
    assert!(
        ingest::extract(&input, "scan.png", "h", &o)
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.kind == "rationale")
    );
    std::fs::write(&script,"import pathlib,sys\nassert sys.argv[2:6]==['--model','base','--output_format','txt']\nassert sys.argv[6]=='--output_dir'\np=pathlib.Path(sys.argv[7])/(pathlib.Path(sys.argv[1]).stem+'.txt')\np.write_text('Decision: because transcription')\n").unwrap();
    let mut recipe = CommandAdapter::whisper("base");
    recipe.program = "python3".into();
    recipe.args.insert(0, script.to_string_lossy().into());
    for ext in ["wav", "mp4"] {
        let input = temp.path().join(format!("recording.{ext}"));
        std::fs::write(&input, b"synthetic").unwrap();
        o.converters.insert(ext.into(), recipe.clone());
        assert!(
            ingest::extract(&input, &format!("recording.{ext}"), "h", &o)
                .unwrap()
                .nodes
                .iter()
                .any(|n| n.kind == "rationale")
        );
    }
    let google = temp.path().join("workbook.gsheet");
    std::fs::write(&google, r#"{"doc_id":"sheet_id"}"#).unwrap();
    std::fs::write(&script,r#"import pathlib,sys,json,zipfile
assert sys.argv[1:5]==['drive','files','export','--params']
p=json.loads(sys.argv[5]); assert p['fileId']=='sheet_id'; assert p['mimeType'].endswith('spreadsheetml.sheet')
assert sys.argv[6]=='-o'
with zipfile.ZipFile(sys.argv[7],'w') as z:
 z.writestr('xl/workbook.xml','<workbook xmlns:r="urn:r"><sheets><sheet name="First" r:id="rId1"/><sheet name="Second" r:id="rId2"/></sheets></workbook>')
 z.writestr('xl/_rels/workbook.xml.rels','<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Target="worksheets/sheet2.xml"/></Relationships>')
 z.writestr('xl/worksheets/sheet1.xml','<worksheet><sheetData><row><c t="inlineStr"><is><t>Name</t></is></c></row></sheetData></worksheet>')
 z.writestr('xl/worksheets/sheet2.xml','<worksheet><sheetData><row><c t="inlineStr"><is><t>Decision: because second sheet</t></is></c></row></sheetData></worksheet>')
 z.writestr('xl/worksheets/_rels/sheet1.xml.rels','<Relationships><Relationship Id="t1" Target="../tables/table1.xml"/></Relationships>')
 z.writestr('xl/tables/table1.xml','<table name="People" displayName="People"><tableColumns><tableColumn name="Name"/></tableColumns></table>')
"#).unwrap();
    let mut recipe = CommandAdapter::google_workspace();
    recipe.program = "python3".into();
    recipe.args.insert(0, script.to_string_lossy().into());
    o.converters.insert("gsheet".into(), recipe);
    let facts = ingest::extract_google(&google, "workbook.gsheet", &o).unwrap();
    for label in ["First", "Second"] {
        assert!(
            facts
                .nodes
                .iter()
                .any(|n| n.kind == "sheet" && n.label == label)
        );
    }
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "table" && n.label == "People")
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "column" && n.label == "Name")
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "rationale" && n.label.contains("second sheet"))
    );
}

#[test]
fn xml_entities_are_preserved_and_incomplete_xml_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("doc.docx");
    zip_file(
        &path,
        &[(
            "word/document.xml",
            "<document><p>Decision: A &amp; B because &#x41; matters</p></document>",
        )],
    );
    let facts = ingest::extract(&path, "doc.docx", "h", &options()).unwrap();
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.label == "Decision: A & B because A matters")
    );
    zip_file(
        &path,
        &[("word/document.xml", "<document><p>Decision: incomplete")],
    );
    assert!(ingest::extract(&path, "doc.docx", "h", &options()).is_err());
}

#[test]
fn markdown_hierarchy_wikilinks_and_qualified_mentions() {
    let f=ingest::extract_text("doc.md","---\r\ntitle: Doc\r\n---\r\n# Outer\n## `Inner`\n[[guide]] and `src/lib.py::Widget::render` and `pkg.Widget`.\n","h",&options()).unwrap();
    let outer = f.nodes.iter().find(|n| n.label == "Outer").unwrap();
    let inner = f.nodes.iter().find(|n| n.label == "Inner").unwrap();
    assert!(
        f.edges
            .iter()
            .any(|e| e.source == outer.id && e.target == inner.id && e.relation == "contains")
    );
    assert!(
        f.references.iter().any(|r| r.source == inner.id
            && r.candidate_keys == ["file:guide.md", "document-wiki:guide.md"])
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.candidate_keys == ["symbol:src/lib.py::Widget.render"])
    );
    assert!(
        f.references
            .iter()
            .any(|r| r.candidate_keys == ["symbol:pkg.Widget"])
    );
    assert_eq!(outer.line, Some(4));
}

#[test]
fn transient_retry_is_bounded_by_call_and_token_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/endpoint", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for status in [429, 200] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut data = vec![];
            let mut buffer = [0; 8192];
            loop {
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                data.extend_from_slice(&buffer[..n]);
                if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&data[..end]);
                    let length: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse().unwrap())
                        })
                        .unwrap();
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let response=if status==200{json!({"choices":[{"finish_reason":"stop","message":{"content":graph().to_string()}}]})}else{json!({"error":"rate limited"})}.to_string();
            write!(stream,"HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
        }
    });
    let mut o = semantic(Provider::OpenAi, endpoint);
    o.semantic.as_mut().unwrap().max_retries = 1;
    o.semantic.as_mut().unwrap().retry_delay_ms = 0;
    let facts = ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
    server.join().unwrap();
    assert!(facts.edges.iter().any(|e| e.relation == "supports"));
}

#[test]
fn opt_in_exact_entity_dedup_and_hyperedges_preserve_evidence() {
    let fragment = json!({"nodes":[{"id":"a","label":"Queue","kind":"concept","evidence":"Queue"},
        {"id":"b","label":"Queue","kind":"concept","evidence":"durability"},{"id":"c","label":"Durability","kind":"concept","evidence":"durability"}],
        "edges":[],"hyperedges":[{"id":"g","label":"Storage","members":["a","c"],"confidence":0.8,"evidence":"Queue provides durability"}]});
    let (endpoint, server) = mock(
        json!({"choices":[{"finish_reason":"stop","message":{"content":fragment.to_string()}}]}),
    );
    let mut o = semantic(Provider::OpenAi, endpoint);
    o.semantic.as_mut().unwrap().deduplicate = true;
    let facts = ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
    server.join().unwrap();
    assert_eq!(facts.nodes.iter().filter(|n| n.label == "Queue").count(), 1);
    let queue = facts.nodes.iter().find(|n| n.label == "Queue").unwrap();
    assert_eq!(
        queue.metadata["corroborating_evidence"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "hyperedge" && n.label == "Storage")
    );
    assert_eq!(
        facts
            .edges
            .iter()
            .filter(|e| e.relation == "member_of")
            .count(),
        2
    );
}

#[cfg(unix)]
#[test]
fn explicit_remote_media_recipe_downloads_then_transcribes() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("download.py");
    std::fs::write(&script,"import pathlib,sys\nassert '--no-playlist' in sys.argv\nassert sys.argv[-2]=='--'\nassert sys.argv[-1]=='http://127.0.0.1:1/explicit-only'\nout=sys.argv[sys.argv.index('--output')+1]\npathlib.Path(out).write_bytes(b'media')\n").unwrap();
    let mut recipe = CommandAdapter::yt_dlp();
    recipe.program = "python3".into();
    recipe.args.insert(0, script.to_string_lossy().into());
    let mut o = options();
    o.allow_private_urls = true;
    o.converters.insert("url".into(), recipe);
    let transcript = python_adapter(
        temp.path(),
        "import sys,pathlib\nassert pathlib.Path(sys.argv[1]).read_bytes()==b'media'\nprint('Decision: because remote audio matters')\n",
    );
    o.converters.insert("webm".into(), transcript);
    let facts = ingest::extract_url("http://127.0.0.1:1/explicit-only", "remote.webm", &o).unwrap();
    assert!(facts.nodes.iter().any(|n| n.kind == "rationale"));
    assert!(
        ingest::extract_url("http://127.0.0.1:1/blocked", "x.txt", &options())
            .unwrap_err()
            .to_string()
            .contains("private/reserved")
    );
}

#[cfg(unix)]
#[test]
fn bedrock_and_claude_cli_use_native_envelopes_and_bounded_settings() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("native.py");
    let response = json!({"stopReason":"end_turn","output":{"message":{"content":[{"text":graph().to_string()}]}}});
    std::fs::write(&script,format!("import sys,json,pathlib\nassert sys.argv[1:3]==['bedrock-runtime','converse']\np=pathlib.Path(sys.argv[sys.argv.index('--cli-input-json')+1].removeprefix('file://'))\nr=json.loads(p.read_text())\nassert r['modelId']=='explicit-bedrock-model'\nassert r['inferenceConfig']['maxTokens']==2048\nassert r['inferenceConfig']['temperature']==1.0\nassert r['inferenceConfig']['topP']==0.9\nassert r['additionalModelRequestFields']['thinking']['budget_tokens']==1024\nassert r['messages'][0]['content'][0]['text']=='Queue provides durability'\nprint({:?})\n",response.to_string())).unwrap();
    let mut adapter = CommandAdapter::bedrock();
    adapter.program = "python3".into();
    adapter.args.insert(0, script.to_string_lossy().into());
    let mut o = options();
    o.semantic = Some(SemanticOptions {
        provider: Provider::Bedrock,
        model: "explicit-bedrock-model".into(),
        temperature: Some(1.0),
        thinking: Some(json!({"type":"enabled","budget_tokens":1024})),
        extra_body: [("inferenceConfig".into(), json!({"topP":0.9}))].into(),
        command: Some(adapter),
        ..Default::default()
    });
    assert!(
        ingest::extract_text("doc.txt", "Queue provides durability", "h", &o)
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "supports")
    );
    let response = json!([{"type":"system"},{"type":"result","subtype":"success","is_error":false,"stop_reason":"end_turn","result":graph().to_string()}]);
    std::fs::write(&script,format!("import os,sys\nassert os.environ['CLAUDE_CODE_MAX_OUTPUT_TOKENS']=='2048'\nassert sys.argv[sys.argv.index('--tools')+1]==''\nassert '--no-session-persistence' in sys.argv\nassert sys.argv[sys.argv.index('--model')+1]=='explicit-claude-model'\nassert 'Queue provides durability' in sys.stdin.read()\nprint({:?})\n",response.to_string())).unwrap();
    let mut adapter = CommandAdapter::claude_cli();
    adapter.program = "python3".into();
    adapter.args.insert(0, script.to_string_lossy().into());
    o.semantic = Some(SemanticOptions {
        provider: Provider::ClaudeCli,
        model: "explicit-claude-model".into(),
        command: Some(adapter),
        ..Default::default()
    });
    assert!(
        ingest::extract_text("doc.txt", "Queue provides durability", "h", &o)
            .unwrap()
            .edges
            .iter()
            .any(|e| e.relation == "supports")
    );
}

fn raw_mock(mime: &str, body: Vec<u8>) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/fixture", listener.local_addr().unwrap());
    let mime = mime.to_owned();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
        }
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
        stream.write_all(&body).unwrap();
        String::from_utf8(request).unwrap()
    });
    (url, server)
}

#[test]
fn remote_format_inference_preserves_identity_and_capture_metadata() {
    let mut o = options();
    o.allow_private_urls = true;
    for (mime,body,format) in [
        ("text/html; charset=utf-8",b"<html><head><title>Useful page</title></head><body><h1>Design</h1><p>Decision: because content matters<script>PRIVATE_SCRIPT_TEXT</script></p></body></html>".to_vec(),"html"),
        ("application/octet-stream",pdf_bytes(),"pdf"),
        ("text/markdown",b"# Native markdown\nDecision: because MIME matters".to_vec(),"md")
    ] {
        let (url,server)=raw_mock(mime,body);let mut facts=ingest::extract_url(&url,"managed/stable-source",&o).unwrap();server.join().unwrap();
        assert_eq!(facts.path,"managed/stable-source");assert_eq!(facts.nodes[0].metadata["format"],format);
        assert!(facts.nodes[0].metadata["captured_at_unix_secs"].as_u64().unwrap()>0);
        assert!(!serde_json::to_string(&facts.nodes).unwrap().contains("PRIVATE_SCRIPT_TEXT"));
        if format=="html" {assert_eq!(facts.nodes[0].metadata["title"],"Useful page");assert!(facts.nodes.iter().any(|n|n.kind=="heading"&&n.label=="Design"));}
        ingest::apply_capture_metadata(&mut facts,&ingest::CaptureMetadata{contributor:Some("Ada".into()),captured_at_unix_secs:Some(123)}).unwrap();
        assert_eq!(facts.nodes[0].metadata["contributor"],"Ada");assert_eq!(facts.nodes[0].metadata["captured_at_unix_secs"],123);
    }
}

#[test]
fn tweet_and_arxiv_metadata_use_only_explicit_mock_endpoints() {
    let tweet = json!({"html":"<blockquote><p>Decision: because evidence matters</p></blockquote><script>NEVER_INDEX</script>","author_name":"Ada Example","author_url":"https://x.com/ada","provider_name":"Twitter"});
    let (endpoint, server) = raw_mock("application/json", tweet.to_string().into_bytes());
    let mut o = options();
    o.allow_private_urls = true;
    o.tweet_oembed_endpoint = Some(endpoint);
    let f = ingest::extract_url("https://x.com/ada/status/123456", "tweets/123456", &o).unwrap();
    let request = server.join().unwrap();
    assert!(request.contains("url=https%3A%2F%2Fx.com%2Fada%2Fstatus%2F123456"));
    assert_eq!(f.nodes[0].metadata["source_type"], "tweet");
    assert_eq!(f.nodes[0].metadata["author_name"], "Ada Example");
    assert_eq!(f.nodes[0].metadata["tweet_id"], "123456");
    assert!(
        !serde_json::to_string(&f.nodes)
            .unwrap()
            .contains("NEVER_INDEX")
    );
    let atom = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"><title>Feed title is not paper title</title><entry><id>http://arxiv.org/abs/2401.12345v2</id><title>A &amp; B: Durable Queues</title><summary>We chose queues because durability matters.</summary><published>2024-01-01T00:00:00Z</published><updated>2024-02-01T00:00:00Z</updated><author><name>Ada Example</name></author><author><name>Bo Example</name></author></entry></feed>"#;
    let (endpoint, server) = raw_mock("application/atom+xml", atom.as_bytes().to_vec());
    o.arxiv_api_endpoint = Some(endpoint);
    let f = ingest::extract_url("https://arxiv.org/abs/2401.12345", "papers/queue", &o).unwrap();
    let request = server.join().unwrap();
    assert!(request.contains("id_list=2401.12345"));
    assert_eq!(f.nodes[0].metadata["title"], "A & B: Durable Queues");
    assert_eq!(
        f.nodes[0].metadata["authors"],
        json!(["Ada Example", "Bo Example"])
    );
    assert_eq!(f.nodes[0].metadata["published"], "2024-01-01T00:00:00Z");
    assert!(f.nodes.iter().any(|n| n.kind == "rationale"));
    let (endpoint, server) = raw_mock(
        "application/atom+xml",
        atom.replace("2401.12345v2", "2401.99999").into_bytes(),
    );
    o.arxiv_api_endpoint = Some(endpoint);
    assert!(ingest::extract_url("https://arxiv.org/abs/2401.12345", "papers/queue", &o).is_err());
    server.join().unwrap();
}

#[test]
fn arxiv_rejects_cross_entry_metadata_and_incomplete_singletons() {
    let complete = "<entry><id>https://arxiv.org/abs/2401.99999</id><title>Other paper</title><summary>Other abstract</summary><author><name>Other Author</name></author></entry>";
    let incomplete = "<entry><id>https://arxiv.org/abs/2401.12345</id></entry>";
    let matching = complete.replace("2401.99999", "2401.12345v2");
    for entries in [
        format!("{complete}{incomplete}"),
        format!("{complete}{matching}"),
        format!("<entry>{matching}</entry>"),
        format!("{matching}<entry/>"),
        incomplete.to_owned(),
    ] {
        let body = format!("<feed xmlns='http://www.w3.org/2005/Atom'>{entries}</feed>");
        let (endpoint, server) = raw_mock("application/atom+xml", body.into_bytes());
        let mut o = options();
        o.allow_private_urls = true;
        o.arxiv_api_endpoint = Some(endpoint);
        assert!(
            ingest::extract_url("https://arxiv.org/abs/2401.12345", "papers/queue", &o).is_err()
        );
        server.join().unwrap();
    }
}

#[test]
fn docx_styles_inherit_heading_levels_and_preserve_ordered_and_bullet_lists() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("styled.docx");
    zip_file(
        &path,
        &[
            (
                "word/styles.xml",
                r#"<w:styles xmlns:w="urn:w"><w:style w:styleId="BaseSection"><w:name w:val="Section"/><w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style><w:style w:styleId="CustomSection"><w:basedOn w:val="BaseSection"/></w:style><w:style w:styleId="ListBullet"><w:name w:val="List Bullet"/></w:style></w:styles>"#,
            ),
            (
                "word/numbering.xml",
                r#"<w:numbering xmlns:w="urn:w"><w:abstractNum w:abstractNumId="7"><w:lvl w:ilvl="0"><w:start w:val="3"/><w:numFmt w:val="decimal"/></w:lvl><w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum><w:num w:numId="42"><w:abstractNumId w:val="7"/></w:num></w:numbering>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="urn:w"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Overview</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val="CustomSection"/></w:pPr><w:r><w:t>Custom section</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="42"/></w:numPr></w:pPr><w:r><w:t>First</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="42"/></w:numPr></w:pPr><w:r><w:t>Second</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="42"/></w:numPr></w:pPr><w:r><w:t>Nested</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val="ListBullet"/></w:pPr><w:r><w:t>Bullet &amp; evidence</w:t></w:r></w:p></w:body></w:document>"#,
            ),
        ],
    );
    let f = ingest::extract(&path, "styled.docx", "h", &options()).unwrap();
    let text = f.nodes[0].metadata["text"].as_str().unwrap();
    assert!(text.contains("# Overview"));
    assert!(text.contains("## Custom section"));
    assert!(text.contains("3. First"));
    assert!(text.contains("4. Second"));
    assert!(text.contains("  - Nested"));
    assert!(text.contains("- Bullet & evidence"));
    assert!(
        f.nodes.iter().any(|n| n.kind == "heading"
            && n.label == "Custom section"
            && n.metadata["level"] == 2)
    );
}

fn semantic_responses(responses: Vec<Value>) -> (String, thread::JoinHandle<Vec<Value>>) {
    semantic_status_responses(responses.into_iter().map(|value| (200, value)).collect())
}

fn semantic_status_responses(
    responses: Vec<(u16, Value)>,
) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/chat", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let mut requests = vec![];
        for (status, response) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = vec![];
            let mut buf = [0; 4096];
            let (start, length) = loop {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let h = String::from_utf8_lossy(&bytes[..end]);
                    let length = h
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break (end + 4, length);
                    }
                }
            };
            requests.push(serde_json::from_slice(&bytes[start..start + length]).unwrap());
            let body = response.to_string();
            write!(
                stream,
                "HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        requests
    });
    (endpoint, server)
}

#[test]
fn truncation_bisects_atomically_and_cached_split_tree_avoids_repeated_calls() {
    let truncated =
        json!({"choices":[{"finish_reason":"length","message":{"content":"{partial"}}]});
    let complete = |label: &str| json!({"choices":[{"finish_reason":"stop","message":{"content":json!({"nodes":[{"id":"n","label":label,"kind":"concept","evidence":label}],"edges":[]}).to_string()}}]});
    let (endpoint, server) =
        semantic_responses(vec![truncated, complete("Alpha"), complete("Bravo")]);
    let cache = tempfile::tempdir().unwrap();
    let mut o = semantic(Provider::OpenAi, endpoint);
    let s = o.semantic.as_mut().unwrap();
    s.max_split_depth = 1;
    s.max_calls = 3;
    s.max_total_output_tokens = 3 * s.max_output_tokens;
    s.cache_dir = Some(cache.path().into());
    let budget = std::sync::Arc::new(ingest::SemanticBudget::new(
        Some(3),
        Some(u64::from(s.max_output_tokens) * 3),
    ));
    s.runtime_budget = Some(budget.clone());
    let first = ingest::extract_text("split.txt", "Alpha\nBravo", "h", &o).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["messages"][1]["content"], "Alpha\nBravo");
    assert_eq!(requests[1]["messages"][1]["content"], "Alpha\n");
    assert_eq!(requests[2]["messages"][1]["content"], "Bravo");
    assert!(
        first
            .nodes
            .iter()
            .any(|n| n.label == "Alpha" && n.line == Some(1))
    );
    assert!(
        first
            .nodes
            .iter()
            .any(|n| n.label == "Bravo" && n.line == Some(2))
    );
    let warm = ingest::extract_text("split.txt", "Alpha\nBravo", "h", &o).unwrap();
    assert_eq!(
        serde_json::to_value(&first.nodes).unwrap(),
        serde_json::to_value(&warm.nodes).unwrap()
    );
    assert_eq!(budget.usage().unwrap().calls, 3);
    assert_eq!(budget.usage().unwrap().reserved_output_tokens, 3 * 2048);
    let zero = std::sync::Arc::new(ingest::SemanticBudget::new(Some(0), Some(0)));
    o.semantic.as_mut().unwrap().runtime_budget = Some(zero.clone());
    ingest::extract_text("another-file.txt", "Alpha\nBravo", "h", &o).unwrap();
    assert_eq!(zero.usage().unwrap().calls, 0);
}

#[test]
fn splitting_does_not_extend_budget_or_recover_invalid_json() {
    for token_limited in [false, true] {
        let (endpoint, server) = semantic_responses(vec![
            json!({"choices":[{"finish_reason":"length","message":{"content":"{partial"}}]}),
        ]);
        let mut o = semantic(Provider::OpenAi, endpoint);
        let s = o.semantic.as_mut().unwrap();
        s.max_split_depth = 2;
        let shared = std::sync::Arc::new(ingest::SemanticBudget::new(Some(3), None));
        s.runtime_budget = Some(shared.clone());
        if token_limited {
            s.max_total_output_tokens = s.max_output_tokens;
        } else {
            s.max_calls = 1;
        }
        let error = ingest::extract_text("split.txt", "Alpha\nBravo", "h", &o).unwrap_err();
        assert!(format!("{error:#}").contains("budget"));
        assert_eq!(server.join().unwrap().len(), 1);
        assert_eq!(shared.usage().unwrap().calls, 1);
    }
    let (endpoint, server) = semantic_responses(vec![
        json!({"choices":[{"finish_reason":"stop","message":{"content":"{invalid JSON"}}]}),
    ]);
    let mut o = semantic(Provider::OpenAi, endpoint);
    o.semantic.as_mut().unwrap().max_split_depth = 3;
    assert!(ingest::extract_text("split.txt", "Alpha\nBravo", "h", &o).is_err());
    assert_eq!(server.join().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn non_unicode_provider_key_errors_do_not_disclose_secret_bytes() {
    const KEY: &str = "GRAF_INGEST_NONUNICODE_TEST_KEY";
    if std::env::var_os("GRAF_INGEST_NONUNICODE_CHILD").is_some() {
        let mut o = semantic(Provider::OpenAi, "http://127.0.0.1:1/must-not-call".into());
        o.semantic.as_mut().unwrap().key_env = Some(KEY.into());
        let error =
            ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("key environment variable"));
        assert!(!message.contains("SYNTHETIC_SECRET"));
        assert!(!format!("{error:?}").contains("SYNTHETIC_SECRET"));
        return;
    }
    use std::os::unix::ffi::OsStringExt;
    let secret = std::ffi::OsString::from_vec(b"SYNTHETIC_SECRET_\xff_END".to_vec());
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "non_unicode_provider_key_errors_do_not_disclose_secret_bytes",
            "--nocapture",
        ])
        .env("GRAF_INGEST_NONUNICODE_CHILD", "1")
        .env(KEY, secret)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SYNTHETIC_SECRET"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SYNTHETIC_SECRET"));
}

#[test]
fn deep_enrichment_preserves_native_ast_facts_and_uses_distinct_ids() {
    let source = "def queue():\n    \"\"\"Queue provides durability\"\"\"\n    return flush()\n";
    let mut facts = graf::parser::parse_python("src/queue.py", source, "code-hash").unwrap();
    let nodes = serde_json::to_value(&facts.nodes).unwrap();
    let edges = serde_json::to_value(&facts.edges).unwrap();
    let references = serde_json::to_value(&facts.references).unwrap();
    let diagnostics = serde_json::to_value(&facts.diagnostics).unwrap();
    let count = facts.nodes.len();
    let edge_count = facts.edges.len();
    ingest::enrich_facts(&mut facts, source, &options()).unwrap();
    assert_eq!(facts.nodes.len(), count);
    let (endpoint, server) = mock(
        json!({"choices":[{"finish_reason":"stop","message":{"content":graph().to_string()}}]}),
    );
    let o = semantic(Provider::OpenAi, endpoint);
    ingest::enrich_facts(&mut facts, source, &o).unwrap();
    server.join().unwrap();
    assert_eq!(serde_json::to_value(&facts.nodes[..count]).unwrap(), nodes);
    assert_eq!(
        serde_json::to_value(&facts.edges[..edge_count]).unwrap(),
        edges
    );
    assert_eq!(serde_json::to_value(&facts.references).unwrap(), references);
    assert_eq!(
        serde_json::to_value(&facts.diagnostics).unwrap(),
        diagnostics
    );
    assert_eq!(facts.hash, "code-hash");
    assert!(
        facts.nodes[count..]
            .iter()
            .all(|n| n.id.starts_with("semantic:") && n.metadata["inferred"] == true)
    );
    assert!(
        facts.edges[edge_count..]
            .iter()
            .all(|e| e.confidence == "inferred")
    );
    assert_eq!(
        facts
            .nodes
            .iter()
            .map(|n| &n.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        facts.nodes.len()
    );
    let before = serde_json::to_value(&facts.nodes).unwrap();
    let (endpoint, server) =
        mock(json!({"choices":[{"finish_reason":"stop","message":{"content":"{invalid"}}]}));
    assert!(
        ingest::enrich_facts(&mut facts, source, &semantic(Provider::OpenAi, endpoint)).is_err()
    );
    server.join().unwrap();
    assert_eq!(serde_json::to_value(&facts.nodes).unwrap(), before);
}

#[test]
fn skill_files_use_markdown_headings_frontmatter_and_symbol_references() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("guide.skill");
    std::fs::write(
        &path,
        "---\ntitle: Queue guide\n---\n# Queue\nUse `Queue`.\n",
    )
    .unwrap();
    assert!(ingest::supports(&path));
    let facts = ingest::extract(&path, "guide.skill", "h", &options()).unwrap();
    assert_eq!(
        facts.nodes[0].metadata["frontmatter"]["title"],
        "Queue guide"
    );
    assert!(
        facts
            .nodes
            .iter()
            .any(|n| n.kind == "heading" && n.label == "Queue" && n.line == Some(4))
    );
    assert!(
        facts
            .references
            .iter()
            .any(|r| r.relation == "documents" && r.candidate_keys == ["symbol:Queue"])
    );
}

#[test]
fn shared_semantic_budget_counts_retries_and_failures_but_not_cross_file_cache_hits() {
    for token_limited in [false, true] {
        let success =
            json!({"choices":[{"finish_reason":"stop","message":{"content":graph().to_string()}}]});
        let (endpoint, server) = semantic_status_responses(vec![
            (429, json!({"error":"retry"})),
            (200, success),
            (
                200,
                json!({"choices":[{"finish_reason":"stop","message":{"content":"{broken"}}]}),
            ),
        ]);
        let cache = tempfile::tempdir().unwrap();
        let mut o = semantic(Provider::OpenAi, endpoint);
        let initial_stamp = ingest::config_fingerprint(&o).unwrap();
        let budget = std::sync::Arc::new(ingest::SemanticBudget::new(
            if token_limited { None } else { Some(3) },
            if token_limited { Some(3 * 2048) } else { None },
        ));
        let s = o.semantic.as_mut().unwrap();
        s.runtime_budget = Some(budget.clone());
        s.cache_dir = Some(cache.path().into());
        assert_eq!(ingest::config_fingerprint(&o).unwrap(), initial_stamp);
        o.semantic.as_mut().unwrap().max_retries = 1;
        o.semantic.as_mut().unwrap().retry_delay_ms = 0;
        let clone = o.clone();
        assert!(std::sync::Arc::ptr_eq(
            clone
                .semantic
                .as_ref()
                .unwrap()
                .runtime_budget
                .as_ref()
                .unwrap(),
            &budget
        ));
        ingest::extract_text("first.txt", "Queue provides durability", "h", &o).unwrap();
        assert_eq!(budget.usage().unwrap().calls, 2);
        assert!(
            ingest::extract_text("second.txt", "Queue provides durability twice", "h", &clone)
                .is_err()
        );
        assert_eq!(server.join().unwrap().len(), 3);
        assert_eq!(budget.usage().unwrap().reserved_output_tokens, 3 * 2048);
        ingest::extract_text("cached-third.txt", "Queue provides durability", "h", &clone).unwrap();
        let error =
            ingest::extract_text("fourth.txt", "Other uncached evidence", "h", &clone).unwrap_err();
        assert!(error.to_string().contains("corpus semantic"));
        assert_eq!(budget.usage().unwrap().calls, 3);
        let zero = std::sync::Arc::new(ingest::SemanticBudget::new(Some(0), Some(0)));
        o.semantic.as_mut().unwrap().runtime_budget = Some(zero.clone());
        ingest::extract_text("warm-zero.txt", "Queue provides durability", "h", &o).unwrap();
        assert_eq!(zero.usage().unwrap().calls, 0);
        assert!(
            !serde_json::to_string(&o)
                .unwrap()
                .contains("runtime_budget")
        );
        o.force_cache_refresh = true;
        assert!(
            ingest::extract_text("force.txt", "Queue provides durability", "h", &o)
                .unwrap_err()
                .to_string()
                .contains("corpus semantic")
        );
        assert_eq!(zero.usage().unwrap().reserved_output_tokens, 0);
    }
}

#[test]
fn provider_controls_use_native_fields_and_reject_managed_overrides_before_requests() {
    for provider in [
        Provider::OpenAi,
        Provider::Azure,
        Provider::Anthropic,
        Provider::Gemini,
        Provider::Ollama,
    ] {
        let content = graph().to_string();
        let response = match provider {
            Provider::OpenAi | Provider::Azure => {
                json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]})
            }
            Provider::Anthropic => {
                json!({"stop_reason":"end_turn","content":[{"type":"thinking","thinking":"not graph JSON"},{"type":"text","text":content}]})
            }
            Provider::Gemini => {
                json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":content}]}}]})
            }
            Provider::Ollama => {
                json!({"done":true,"done_reason":"stop","message":{"thinking":"not graph JSON","content":content}})
            }
            _ => unreachable!(),
        };
        let (endpoint, server) = mock(response);
        let mut o = semantic(provider, endpoint);
        let before = ingest::config_fingerprint(&o).unwrap();
        let s = o.semantic.as_mut().unwrap();
        s.model = "explicit-compatible-model".into();
        s.temperature = Some(1.0);
        s.thinking = Some(match provider {
            Provider::Anthropic => json!({"type":"enabled","budget_tokens":1024}),
            Provider::Gemini => json!({"thinkingBudget":512}),
            Provider::Ollama => json!(true),
            _ => json!("high"),
        });
        let (key, value) = match provider {
            Provider::Gemini => ("generationConfig", json!({"topK":20})),
            Provider::Ollama => ("options", json!({"seed":42})),
            _ => ("top_p", json!(0.9)),
        };
        s.extra_body.insert(key.into(), value);
        ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
        assert_ne!(ingest::config_fingerprint(&o).unwrap(), before);
        let request = server.join().unwrap();
        match provider {
            Provider::OpenAi | Provider::Azure => {
                assert_eq!(request["temperature"], 1.0);
                assert_eq!(request["reasoning_effort"], "high");
                assert_eq!(request["max_completion_tokens"], 2048);
            }
            Provider::Anthropic => {
                assert_eq!(request["thinking"]["budget_tokens"], 1024);
                assert_eq!(request["max_tokens"], 2048);
            }
            Provider::Gemini => {
                assert_eq!(
                    request["generationConfig"]["thinkingConfig"]["thinkingBudget"],
                    512
                );
                assert_eq!(request["generationConfig"]["temperature"], 1.0);
                assert_eq!(request["generationConfig"]["topK"], 20);
                assert_eq!(request["generationConfig"]["maxOutputTokens"], 2048);
            }
            Provider::Ollama => {
                assert_eq!(request["think"], true);
                assert_eq!(request["options"]["temperature"], 1.0);
                assert_eq!(request["options"]["seed"], 42);
                assert_eq!(request["options"]["num_predict"], 2048);
            }
            _ => unreachable!(),
        }
    }
    for extra in [
        json!({"messages":[]}),
        json!({"n":2}),
        json!({"generationConfig":{"maxOutputTokens":99999}}),
        json!({"options":{"num_predict":-1}}),
        json!({"inferenceConfig":null}),
        json!({"nested":{"api_key":"SYNTHETIC_SECRET"}}),
    ] {
        let mut o = semantic(Provider::OpenAi, "http://127.0.0.1:1/no-call".into());
        o.semantic.as_mut().unwrap().extra_body = serde_json::from_value(extra).unwrap();
        let error = ingest::extract_text("doc.txt", "Queue", "h", &o).unwrap_err();
        assert!(!format!("{error:#}").contains("SYNTHETIC_SECRET"));
        assert!(ingest::config_fingerprint(&o).is_err());
    }
    let mut o = semantic(Provider::Anthropic, "http://127.0.0.1:1/no-call".into());
    o.semantic.as_mut().unwrap().thinking = Some(json!({"type":"enabled","budget_tokens":2048}));
    assert!(ingest::config_fingerprint(&o).is_err());
}

#[test]
fn semantic_cache_inspection_repair_and_runtime_force_are_explicit() {
    let success =
        json!({"choices":[{"finish_reason":"stop","message":{"content":graph().to_string()}}]});
    let (endpoint, server) = semantic_responses(vec![success.clone(), success.clone(), success]);
    let cache = tempfile::tempdir().unwrap();
    let mut o = semantic(Provider::OpenAi, endpoint);
    o.semantic.as_mut().unwrap().cache_dir = Some(cache.path().into());
    let budget = std::sync::Arc::new(ingest::SemanticBudget::new(Some(3), None));
    o.semantic.as_mut().unwrap().runtime_budget = Some(budget.clone());
    ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
    let entries = ingest::inspect_semantic_cache(cache.path(), 10, 1024 * 1024).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(matches!(
        entries[0].status,
        ingest::SemanticCacheStatus::Graph
    ));
    assert!(
        !serde_json::to_string(&entries)
            .unwrap()
            .contains("durability")
    );
    let path = cache.path().join(format!("{}.json", entries[0].key));
    std::fs::write(&path, "{broken SYNTHETIC_PRIVATE_CONTENT").unwrap();
    let invalid = ingest::inspect_semantic_cache(cache.path(), 10, 1024 * 1024).unwrap();
    assert!(matches!(
        invalid[0].status,
        ingest::SemanticCacheStatus::Invalid(_)
    ));
    assert!(
        !serde_json::to_string(&invalid)
            .unwrap()
            .contains("SYNTHETIC_PRIVATE_CONTENT")
    );
    assert!(ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).is_err());
    assert_eq!(budget.usage().unwrap().calls, 1);
    assert!(ingest::remove_semantic_cache_entry(cache.path(), &entries[0].key).unwrap());
    assert!(!ingest::remove_semantic_cache_entry(cache.path(), &entries[0].key).unwrap());
    assert!(ingest::remove_semantic_cache_entry(cache.path(), "../outside").is_err());
    ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
    let fingerprint = ingest::config_fingerprint(&o).unwrap();
    o.force_cache_refresh = true;
    assert_eq!(ingest::config_fingerprint(&o).unwrap(), fingerprint);
    assert!(
        !serde_json::to_string(&o)
            .unwrap()
            .contains("force_cache_refresh")
    );
    ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
    assert_eq!(server.join().unwrap().len(), 3);
    assert_eq!(budget.usage().unwrap().calls, 3);
    assert!(
        ingest::inspect_semantic_cache(cache.path(), 10, 1)
            .unwrap()
            .iter()
            .all(|e| matches!(e.status, ingest::SemanticCacheStatus::Invalid(_)))
    );
    std::fs::write(
        cache.path().join(format!("{}.json", "a".repeat(64))),
        r#"{"split_at":3}"#,
    )
    .unwrap();
    assert!(
        ingest::inspect_semantic_cache(cache.path(), 10, 1024)
            .unwrap()
            .iter()
            .any(|e| matches!(e.status, ingest::SemanticCacheStatus::Split))
    );
    assert!(ingest::inspect_semantic_cache(cache.path(), 1, 1024).is_err());
}

#[cfg(unix)]
#[test]
fn transcript_cache_reuses_bytes_and_keys_explicit_whisper_model_prompt() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("transcripts");
    let script = temp.path().join("whisper.py");
    let counter = temp.path().join("calls");
    std::fs::write(
        &script,
        r#"
import pathlib, sys
counter = pathlib.Path(sys.argv[1])
counter.write_text(str(int(counter.read_text()) + 1) if counter.exists() else '1')
args = sys.argv[2:]
assert args[args.index('--model')+1] == 'base'
prompt = args[args.index('--initial_prompt')+1]
assert args[args.index('--output_format')+1] == 'txt'
output = pathlib.Path(args[args.index('--output_dir')+1]) / (pathlib.Path(args[0]).stem + '.txt')
output.write_text('Decision: because ' + prompt + ' matters.')
"#,
    )
    .unwrap();
    let mut adapter = CommandAdapter::whisper_with_prompt("base", "Queue terminology");
    adapter.program = "python3".into();
    adapter.args.splice(
        0..0,
        [
            script.to_string_lossy().into_owned(),
            counter.to_string_lossy().into_owned(),
        ],
    );
    let mut o = options();
    o.converters.insert("mp3".into(), adapter);
    let fingerprint = ingest::config_fingerprint(&o).unwrap();
    o.transcript_cache_dir = Some(cache.clone());
    assert_eq!(ingest::config_fingerprint(&o).unwrap(), fingerprint);
    let one = temp.path().join("one.mp3");
    let two = temp.path().join("two.mp3");
    std::fs::write(&one, b"synthetic audio").unwrap();
    std::fs::write(&two, b"synthetic audio").unwrap();
    let first = ingest::extract(&one, "one.mp3", "h", &o).unwrap();
    let second = ingest::extract(&two, "two.mp3", "h", &o).unwrap();
    assert_eq!(
        first.nodes[0].metadata["text"],
        second.nodes[0].metadata["text"]
    );
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "1");
    for entry in std::fs::read_dir(&cache).unwrap() {
        assert_eq!(
            entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    o.force_cache_refresh = true;
    ingest::extract(&one, "one.mp3", "h", &o).unwrap();
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "2");
    o.force_cache_refresh = false;
    *o.converters
        .get_mut("mp3")
        .unwrap()
        .args
        .last_mut()
        .unwrap() = "Different terminology".into();
    let changed = ingest::extract(&one, "one.mp3", "h", &o).unwrap();
    assert!(
        changed.nodes[0].metadata["text"]
            .as_str()
            .unwrap()
            .contains("Different terminology")
    );
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "3");
    std::fs::write(&two, b"changed audio").unwrap();
    ingest::extract(&two, "two.mp3", "h", &o).unwrap();
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "4");
    std::fs::write(&script, "raise SystemExit(2)").unwrap();
    o.force_cache_refresh = true;
    assert!(ingest::extract(&two, "two.mp3", "h", &o).is_err());
    o.force_cache_refresh = false;
    ingest::extract(&two, "two.mp3", "h", &o).unwrap();
    o.max_text_bytes = 1;
    assert!(ingest::extract(&two, "two.mp3", "h", &o).is_err());
}

#[test]
fn extensionless_markdown_links_offer_local_literal_and_markdown_candidates() {
    let facts = ingest::extract_text(
        "docs/note.md",
        "[Guide](../guide#usage) [Typed](../typed.mdx) [Outside](../../outside) [Self](#here)",
        "h",
        &options(),
    )
    .unwrap();
    assert!(
        facts
            .references
            .iter()
            .any(|r| r.label == "Guide" && r.candidate_keys == ["file:guide", "file:guide.md"])
    );
    assert!(
        facts
            .references
            .iter()
            .any(|r| r.label == "Typed" && r.candidate_keys == ["file:typed.mdx"])
    );
    assert!(!facts.references.iter().any(|r| r.label == "Outside"));
    assert!(
        facts
            .references
            .iter()
            .any(|r| r.label == "Self" && r.candidate_keys == ["file:docs/note.md"])
    );
}

#[test]
fn markdown_wikilinks_publish_bounded_suffix_aliases_and_ordered_candidates() {
    let target =
        ingest::extract_text("Vault/Materials/ref.md", "Reference", "h", &options()).unwrap();
    assert_eq!(
        target.nodes[0].binding_key.as_deref(),
        Some("file:Vault/Materials/ref.md")
    );
    assert_eq!(
        target.nodes[0].metadata["binding_aliases"],
        json!([
            "document-wiki:Vault/Materials/ref.md",
            "document-wiki:Materials/ref.md",
            "document-wiki:ref.md"
        ])
    );
    let text = format!(
        "[[./Materials/../hub#usage|Hub alias]] [[Materials/ref#Details|Ref alias]] \
         [[../hub]] [[#here|Self]] [[../../outside]] [[/absolute]] \
         [[https://example.test/page]] [[C:\\secret]] [[folder/..]] [[{}]] [[{}end]]",
        "a".repeat(4097),
        "part/".repeat(257),
    );
    let facts = ingest::extract_text("log/entry.md", &text, "h", &options()).unwrap();
    assert_eq!(facts.references.len(), 4);
    for (label, keys) in [
        (
            "Hub alias",
            vec!["file:log/hub.md", "file:hub.md", "document-wiki:hub.md"],
        ),
        (
            "Ref alias",
            vec![
                "file:log/Materials/ref.md",
                "file:Materials/ref.md",
                "document-wiki:Materials/ref.md",
            ],
        ),
        ("../hub", vec!["file:hub.md"]),
        ("Self", vec!["file:log/entry.md"]),
    ] {
        let reference = facts.references.iter().find(|r| r.label == label).unwrap();
        assert_eq!(reference.candidate_keys, keys);
    }
    assert!(!facts.nodes.iter().any(|n| n.kind == "url"));
    let plain = ingest::extract_text("ref.txt", "Reference", "h", &options()).unwrap();
    assert!(plain.nodes[0].metadata.get("binding_aliases").is_none());
    assert!(
        ingest::config_fingerprint(&options())
            .unwrap()
            .starts_with("ingest-v6:")
    );
}

fn indexed_document_targets(db: &Path, source: &str) -> Vec<String> {
    let graph = graf::store::Store::open_read_only(db)
        .unwrap()
        .snapshot()
        .unwrap();
    let nodes: std::collections::HashMap<_, _> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut targets = Vec::new();
    for edge in &graph.edges {
        let from = nodes
            .get(edge.source.as_str())
            .expect("indexed edge source exists");
        let to = nodes
            .get(edge.target.as_str())
            .expect("indexed edge target exists");
        if from.file == source && edge.relation == "references" && to.kind == "document" {
            targets.push(to.file.clone());
        }
    }
    targets.sort();
    targets.dedup();
    targets
}

#[test]
fn indexed_wikilink_suffix_fallback_rebinds_after_moves_ambiguity_and_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("corpus");
    let db = temp.path().join("graph.db");
    for directory in [
        "log",
        "Materials",
        "Vault/Materials",
        "Far/Nested/Materials",
        "OtherMaterials",
    ] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    for (path, text) in [
        ("hub.md", "Root hub"),
        ("Vault/Materials/ref.md", "Reference"),
        ("OtherMaterials/ref.md", "Different path segment"),
        (
            "log/wiki.md",
            "[[hub]] and [[Materials/ref#Details|Reference]]",
        ),
        ("log/base.md", "[[ref]]"),
        (
            "log/inline.md",
            "[hub](hub.md) and [reference](Materials/ref.md)",
        ),
        ("log/relative.md", "[hub](../hub.md)"),
    ] {
        std::fs::write(root.join(path), text).unwrap();
    }
    graf::index::run(&root, &db).unwrap();
    assert_eq!(
        indexed_document_targets(&db, "log/wiki.md"),
        ["Vault/Materials/ref.md", "hub.md"]
    );
    assert!(indexed_document_targets(&db, "log/base.md").is_empty());
    assert!(indexed_document_targets(&db, "log/inline.md").is_empty());
    assert_eq!(indexed_document_targets(&db, "log/relative.md"), ["hub.md"]);

    std::fs::rename(
        root.join("Vault/Materials/ref.md"),
        root.join("Far/Nested/Materials/ref.md"),
    )
    .unwrap();
    let moved = graf::index::run(&root, &db).unwrap();
    assert_eq!(moved.parsed_files, 1);
    assert_eq!(moved.deleted_files, 1);
    assert!(moved.unchanged_files >= 4);
    assert_eq!(
        indexed_document_targets(&db, "log/wiki.md"),
        ["Far/Nested/Materials/ref.md", "hub.md"]
    );

    // Different depths remain ambiguous; no shallowest-path heuristic.
    std::fs::write(root.join("Vault/Materials/ref.md"), "Competing reference").unwrap();
    graf::index::run(&root, &db).unwrap();
    assert_eq!(indexed_document_targets(&db, "log/wiki.md"), ["hub.md"]);
    std::fs::write(root.join("Materials/ref.md"), "Exact path from the root").unwrap();
    graf::index::run(&root, &db).unwrap();
    assert_eq!(
        indexed_document_targets(&db, "log/wiki.md"),
        ["Materials/ref.md", "hub.md"]
    );
    std::fs::remove_file(root.join("Materials/ref.md")).unwrap();
    graf::index::run(&root, &db).unwrap();
    assert_eq!(indexed_document_targets(&db, "log/wiki.md"), ["hub.md"]);
    std::fs::remove_file(root.join("Far/Nested/Materials/ref.md")).unwrap();
    graf::index::run(&root, &db).unwrap();
    assert_eq!(
        indexed_document_targets(&db, "log/wiki.md"),
        ["Vault/Materials/ref.md", "hub.md"]
    );
    std::fs::remove_file(root.join("Vault/Materials/ref.md")).unwrap();
    graf::index::run(&root, &db).unwrap();
    assert_eq!(indexed_document_targets(&db, "log/wiki.md"), ["hub.md"]);
    assert_eq!(
        indexed_document_targets(&db, "log/base.md"),
        ["OtherMaterials/ref.md"]
    );
    assert!(indexed_document_targets(&db, "log/inline.md").is_empty());
}

#[test]
fn indexed_wikilink_sibling_then_root_priority_preserves_ordinary_relative_links() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("corpus");
    let db = temp.path().join("graph.db");
    for directory in ["log", "notes", "Deep/Nested"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    for (path, text) in [
        ("hub.md", "Root"),
        ("log/hub.md", "Sibling"),
        ("Deep/Nested/hub.md", "Nested"),
        ("log/entry.md", "[[hub#Details|Hub]]"),
        ("notes/entry.md", "[[hub]]"),
        ("notes/inline.md", "[hub](hub.md) and [extensionless](hub)"),
    ] {
        std::fs::write(root.join(path), text).unwrap();
    }
    graf::index::run(&root, &db).unwrap();
    assert_eq!(
        indexed_document_targets(&db, "log/entry.md"),
        ["log/hub.md"]
    );
    assert_eq!(indexed_document_targets(&db, "notes/entry.md"), ["hub.md"]);
    assert!(indexed_document_targets(&db, "notes/inline.md").is_empty());

    std::fs::remove_file(root.join("hub.md")).unwrap();
    graf::index::run(&root, &db).unwrap();
    assert!(indexed_document_targets(&db, "notes/entry.md").is_empty());
    assert_eq!(
        indexed_document_targets(&db, "log/entry.md"),
        ["log/hub.md"]
    );
    std::fs::remove_file(root.join("log/hub.md")).unwrap();
    graf::index::run(&root, &db).unwrap();
    for source in ["log/entry.md", "notes/entry.md"] {
        assert_eq!(
            indexed_document_targets(&db, source),
            ["Deep/Nested/hub.md"]
        );
    }
    assert!(indexed_document_targets(&db, "notes/inline.md").is_empty());

    std::fs::write(root.join("hub.md"), "Restored root").unwrap();
    graf::index::run(&root, &db).unwrap();
    assert_eq!(indexed_document_targets(&db, "notes/entry.md"), ["hub.md"]);
    std::fs::write(root.join("notes/entry.md"), "[hub](hub.md)").unwrap();
    let updated = graf::index::run(&root, &db).unwrap();
    assert_eq!(updated.parsed_files, 1);
    assert!(indexed_document_targets(&db, "notes/entry.md").is_empty());
    std::fs::write(root.join("notes/hub.md"), "Ordinary relative destination").unwrap();
    graf::index::run(&root, &db).unwrap();
    for source in ["notes/entry.md", "notes/inline.md"] {
        assert_eq!(indexed_document_targets(&db, source), ["notes/hub.md"]);
    }
}

#[test]
fn docx_tables_preserve_order_cells_and_spans_without_duplicate_paragraphs() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("tables.docx");
    let xml = r#"<w:document xmlns:w="urn:w"><w:body>
<w:p><w:r><w:t>Before table</w:t></w:r></w:p>
<w:tbl><w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>Pipe | &amp; tag &lt;b&gt;</w:t></w:r></w:p><w:p><w:r><w:t>Second paragraph</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>Merged</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>After table</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Unmarked row</w:t></w:r></w:p></w:tc><w:tc/></w:tr></w:tbl>
</w:body></w:document>"#;
    zip_file(&path, &[("word/document.xml", xml)]);
    let facts = ingest::extract(&path, "tables.docx", "h", &options()).unwrap();
    let text = facts.nodes[0].metadata["text"].as_str().unwrap();
    assert!(text.contains("| Name | Value |\n| --- | --- |"));
    assert!(text.contains("Pipe \\| &amp; tag &lt;b&gt;<br>Second paragraph | 42"));
    assert!(text.contains("| Merged |  |"));
    assert_eq!(text.matches("Second paragraph").count(), 1);
    assert!(text.find("Before table").unwrap() < text.find("| Name").unwrap());
    assert!(text.find("| Merged").unwrap() < text.find("## After table").unwrap());
    assert!(text.contains("|  |  |\n| --- | --- |\n| Unmarked row |  |"));
    zip_file(
        &path,
        &[(
            "word/document.xml",
            &xml.replace("w:val=\"2\"", "w:val=\"999999\""),
        )],
    );
    assert!(ingest::extract(&path, "tables.docx", "h", &options()).is_err());
}

fn redirect_mock(responses: Vec<(u16, String, u64)>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/start", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let mut requests = vec![];
        for (status, location, delay_ms) in responses {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "redirect fixture not reached"
                        );
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut request = vec![];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut b = [0];
                stream.read_exact(&mut b).unwrap();
                request.push(b[0]);
                assert!(request.len() < 8192);
            }
            requests.push(String::from_utf8(request).unwrap());
            thread::sleep(std::time::Duration::from_millis(delay_ms));
            let location = if location.is_empty() {
                String::new()
            } else {
                format!("Location: {location}\r\n")
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} Result\r\n{location}Content-Type: text/markdown\r\nContent-Length: 7\r\nConnection: close\r\n\r\n# Final"
            );
        }
        requests
    });
    (url, task)
}

#[test]
fn explicit_url_redirects_are_bounded_and_share_the_original_deadline() {
    let mut o = options();
    o.allow_private_urls = true;
    let mut chain: Vec<_> = (1..=5).map(|i| (302, format!("/hop{i}"), 0)).collect();
    chain.push((200, String::new(), 0));
    let (url, server) = redirect_mock(chain);
    let facts = ingest::extract_url(&url, "managed/source", &o).unwrap();
    assert_eq!(server.join().unwrap().len(), 6);
    assert_eq!(facts.nodes[0].metadata["source_url"], url);
    assert!(
        facts.nodes[0].metadata["retrieved_url"]
            .as_str()
            .unwrap()
            .ends_with("/hop5")
    );
    assert!(facts.nodes.iter().any(|n| n.label == "Final"));
    for (responses, expected) in [
        (
            (1..=6)
                .map(|i| (302, format!("/hop{i}"), 0))
                .collect::<Vec<_>>(),
            "redirect limit",
        ),
        (
            vec![(302, "/again".into(), 0), (302, "/start".into(), 0)],
            "redirect loop",
        ),
        (
            vec![(302, "https://user:SYNTHETIC_SECRET@example.test/".into(), 0)],
            "credentials",
        ),
        (vec![(302, String::new(), 0)], "Location"),
    ] {
        let (url, server) = redirect_mock(responses);
        let error = ingest::extract_url(&url, "managed/source", &o).unwrap_err();
        server.join().unwrap();
        assert!(error.to_string().contains(expected), "{error:#}");
        assert!(!format!("{error:#}").contains("SYNTHETIC_SECRET"));
    }
    o.timeout_secs = 1;
    let (url, server) = redirect_mock(vec![(302, "/next".into(), 500), (200, String::new(), 700)]);
    assert!(ingest::extract_url(&url, "managed/source", &o).is_err());
    assert_eq!(server.join().unwrap().len(), 2);
}

#[cfg(unix)]
#[test]
fn generic_cli_receives_controls_and_claude_cli_rejects_unsupported_controls() {
    let temp = tempfile::tempdir().unwrap();
    let adapter = python_adapter(
        temp.path(),
        &format!(
            "import json,sys\nr=json.load(sys.stdin)\nassert r['temperature']==0.5\nassert r['thinking']=={{'mode':'adapter-native'}}\nassert r['seed']==42\nassert r['max_output_tokens']==2048\nprint({:?})\n",
            graph().to_string()
        ),
    );
    let mut o = options();
    o.semantic = Some(SemanticOptions {
        provider: Provider::Cli,
        command: Some(adapter),
        temperature: Some(0.5),
        thinking: Some(json!({"mode":"adapter-native"})),
        extra_body: [("seed".into(), json!(42))].into(),
        ..Default::default()
    });
    ingest::extract_text("doc.txt", "Queue provides durability", "h", &o).unwrap();
    o.semantic.as_mut().unwrap().provider = Provider::ClaudeCli;
    assert!(
        ingest::config_fingerprint(&o)
            .unwrap_err()
            .to_string()
            .contains("Claude CLI")
    );
}
