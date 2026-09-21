use graf::{index, model::GraphSnapshot, store::Store};
use std::fs;
use tempfile::tempdir;

fn edge(graph: &GraphSnapshot, source: &str, target: &str, relation: &str) -> bool {
    graph.edges.iter().any(|edge| {
        edge.relation == relation
            && graph.nodes.iter().any(|node| {
                node.id == edge.source && node.qualified_name.as_deref() == Some(source)
            })
            && graph.nodes.iter().any(|node| {
                node.id == edge.target && node.qualified_name.as_deref() == Some(target)
            })
    })
}

#[test]
fn overridden_receiver_retains_declaration_without_guessing_runtime_target() {
    let dir = tempdir().unwrap();
    let db = dir.path().join(".graf/index.db");
    fs::write(
        dir.path().join("base.py"),
        "class Reader:\n    def read(self): return self.parse()\n    def parse(self): return 1\n",
    )
    .unwrap();
    let child = dir.path().join("child.py");
    fs::write(
        &child,
        "from base import Reader\nclass Other(Reader):\n    def parse(self): return 2\n",
    )
    .unwrap();
    index::run(dir.path(), &db).unwrap();
    let graph = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(edge(
        &graph,
        "Reader.read",
        "Reader.parse",
        "declared_member"
    ));
    assert!(!edge(&graph, "Reader.read", "Reader.parse", "calls"));
    assert!(!edge(&graph, "Reader.read", "Other.parse", "calls"));
    assert_eq!(index::run(dir.path(), &db).unwrap().parsed_files, 0);

    fs::remove_file(&child).unwrap();
    index::run(dir.path(), &db).unwrap();
    let graph = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(edge(&graph, "Reader.read", "Reader.parse", "calls"));
    assert!(!edge(
        &graph,
        "Reader.read",
        "Reader.parse",
        "declared_member"
    ));
}

#[test]
fn uncertain_receiver_members_do_not_gain_declaration_links() {
    for source in [
        "class Reader:\n    def read(self): return self.parse()\n    def parse(self): return 1\n    def mutate(self, value): self.parse = value\nclass Other(Reader):\n    def parse(self): return 2\n",
        "class Reader:\n    def read(self): return self.parse()\n    @property\n    def parse(self): return lambda: 1\nclass Other(Reader):\n    def parse(self): return 2\n",
        "class Reader(unknown):\n    def read(self): return self.parse()\n    def parse(self): return 1\nclass Other(Reader):\n    def parse(self): return 2\n",
        "class Reader:\n    def read(self, other): return other.parse()\n    def parse(self): return 1\nclass Other(Reader):\n    def parse(self): return 2\n",
    ] {
        let dir = tempdir().unwrap();
        let db = dir.path().join(".graf/index.db");
        fs::write(dir.path().join("example.py"), source).unwrap();
        index::run(dir.path(), &db).unwrap();
        let graph = Store::open_read_only(&db).unwrap().snapshot().unwrap();
        assert!(
            !edge(&graph, "Reader.read", "Reader.parse", "declared_member"),
            "{source}"
        );
        assert!(!edge(&graph, "Reader.read", "Reader.parse", "calls"));
    }
}
