# Graf and Graphify performance

The README chart compares Graf with Graphify on the complete Visual Studio Code repository. On this workload, Graf searched about **1000x faster** and refreshed an unchanged graph about **89x faster**.

## Results

| Full VS Code operation | Graf | Graphify | Result |
| --- | ---: | ---: | ---: |
| Search, median of three terms | 0.013 s | 13.264 s | Graf 1006x faster |
| No-op update | 0.88 s | 77.97 s | Graf 88.6x faster |
| One-file comment update | 61.80 s | 75.91 s | Graf 1.23x faster |
| Cold index | 260.10 s | 259.81 s | Effectively tied |
| Cold peak memory | 5.46 GB | 7.92 GB | Graf used 31% less |

The three measured searches were `URI`, `Workbench`, and `ExtensionHost`:

| Search | Graf | Graphify | Speedup |
| --- | ---: | ---: | ---: |
| `URI` | 29.7 ms | 13.264 s | 446x |
| `Workbench` | 13.2 ms | 14.000 s | 1062x |
| `ExtensionHost` | 10.9 ms | 12.919 s | 1190x |

Graf reached cold-index parity while retaining a substantially richer graph:

| Output | Graf | Graphify | Graf / Graphify |
| --- | ---: | ---: | ---: |
| Nodes | 563,959 | 237,986 | 2.37x |
| Edges | 1,559,361 | 900,832 | 1.73x |
| References retained by Graf | 2,538,660 | n/a | — |

## Method

- Machine: Apple M1 Mac mini (`Macmini9,1`), 16 GB RAM, macOS 26.2.
- Corpus: the full VS Code repository at `bf715befd589605be045505d0456e4246054566f`, containing 19,036 regular files.
- Graf: `766c7e9f05cfabbac03d765fa35a38c34128e1fb`.
- Graphify: `b9cd9570728a5ff3485d2a1e36fe9a1272a368ae` (0.9.64).
- Both tools used code-only mode with clustering disabled.
- Each search term was warmed first. Graf used 20 measured samples per term; Graphify used five. The headline search number is the median result across the three terms.
- No-op and one-file updates reused each tool's existing output and incremental cache.
- Wall time covers process launch through exit. Peak memory is the child process's maximum resident set size.

The equivalent commands were:

```text
graf --db STATE/index.db --json index . --code-only --no-semantic
python -m graphify extract . --out STATE/graphify-out --code-only --no-cluster --timing

graf --db STATE/index.db --json query TERM --depth 1 --limit 100
python -m graphify query TERM --graph STATE/graphify-out/graph.json
```

## Tradeoffs

Graf stores a persistent indexed graph so later searches and updates do not reload and traverse a large JSON graph. That index uses more disk: Graf's SQLite database was 6.28 GB, while Graphify's unindexed graph JSON was 496 MB.

The headline numbers describe this repository, these versions, and this machine. Absolute times will vary across machines and codebases. The search comparison measures each product's normal query path; it does not claim that every possible query is exactly 1000x faster.
