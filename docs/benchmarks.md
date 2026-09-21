# Graf and Graphify performance

The short version is simple: Graf indexed about **7x faster** and searched about **40x faster** than Graphify 0.9.65 in the comparison behind the README charts.

## What the numbers mean

The comparison used selected, frozen source projections from eight public repositories on the same Linux host:

- chi
- clap
- click
- commander.js
- httpx
- mux
- ripgrep
- Zod

The run used Linux 6.8 on x86-64 with 32 logical CPUs and glibc 2.39. Each measurement started a fresh process and included process startup through exit. The repositories were already checked out; a cold product run began with an empty product database, cache, and output. Filesystem cache state was left alone, with no manual cache drop or warmup.

Each product ran three times per workload, with product order alternated by repository, stage, and repeat. The result for a workload is the median wall time. A pair counted toward the headline only when both products finished and returned the expected graph.

The normalized commands were:

```text
graf --db STATE/index.db --json index . --code-only --no-semantic
python -m graphify extract . --code-only --no-cluster --timing

graf --db STATE/index.db --json query SYMBOL --depth 1 --limit 100
python -m graphify query SYMBOL --graph graphify-out/graph.json
```

Wall time covers fresh process launch through exit. Peak RSS is GNU `time`/`wait4` maximum RSS for that child process; it does not include simultaneous memory in a separate worker pool.

The indexing set covered cold indexing, no-op updates, body edits, file additions, and file deletions. Nineteen repo/workload pairs were mutually successful and correct. Graf was faster in 18 of them; the geometric mean of the Graphify-to-Graf time ratios was **6.95x**, rounded to **7x**.

The search set covered positive symbol queries after cold indexing, additions, and deletions. Nineteen pairs were mutually successful and correct. Graf was faster and used less peak memory in all 19. The geometric mean wall-time speedup was **41.45x**, rounded to **40x**.

## Contributing results

These are the median wall times used in the two geometric means. A speedup below 1x means Graf was slower.

### Indexing

| Repository | Stage | Graf | Graphify | Speedup |
| --- | --- | ---: | ---: | ---: |
| chi | cold | 0.170s | 0.824s | 4.85x |
| chi | no-op | 0.038s | 0.672s | 17.62x |
| chi | body edit | 0.070s | 0.773s | 11.04x |
| chi | add | 0.039s | 0.773s | 19.68x |
| chi | delete | 0.043s | 0.772s | 18.14x |
| clap | cold | 1.226s | 1.071s | 0.87x |
| clap | no-op | 0.271s | 0.826s | 3.05x |
| clap | body edit | 0.623s | 1.374s | 2.20x |
| clap | delete | 0.536s | 1.428s | 2.66x |
| mux | cold | 0.072s | 0.671s | 9.32x |
| mux | no-op | 0.023s | 0.622s | 27.53x |
| mux | body edit | 0.071s | 0.672s | 9.42x |
| mux | add | 0.023s | 0.720s | 31.74x |
| mux | delete | 0.023s | 0.725s | 31.39x |
| ripgrep | cold | 1.173s | 1.274s | 1.09x |
| ripgrep | no-op | 0.221s | 0.721s | 3.26x |
| ripgrep | body edit | 0.221s | 1.423s | 6.44x |
| ripgrep | add | 0.269s | 1.424s | 5.29x |
| ripgrep | delete | 0.221s | 1.423s | 6.44x |

### Search

| Repository | Stage | Graf | Graphify | Speedup |
| --- | --- | ---: | ---: | ---: |
| chi | cold | 0.014s | 0.571s | 41.93x |
| chi | add | 0.014s | 0.621s | 43.34x |
| chi | delete | 0.014s | 0.624s | 43.80x |
| clap | cold | 0.013s | 0.671s | 50.12x |
| clap | add | 0.021s | 0.723s | 34.66x |
| clap | delete | 0.014s | 0.722s | 51.04x |
| click | cold | 0.013s | 0.569s | 43.44x |
| click | add | 0.014s | 0.570s | 41.05x |
| click | delete | 0.013s | 0.570s | 43.79x |
| commander.js | cold | 0.014s | 0.520s | 37.46x |
| commander.js | add | 0.014s | 0.572s | 40.32x |
| commander.js | delete | 0.013s | 0.519s | 39.58x |
| httpx | cold | 0.014s | 0.618s | 44.30x |
| mux | cold | 0.017s | 0.572s | 32.83x |
| mux | add | 0.017s | 0.621s | 35.66x |
| mux | delete | 0.014s | 0.572s | 40.47x |
| ripgrep | cold | 0.014s | 0.671s | 48.11x |
| ripgrep | add | 0.021s | 0.723s | 33.93x |
| ripgrep | delete | 0.014s | 0.671s | 47.98x |

## Why Graf is faster

Graf stores a persistent graph in SQLite with dedicated full-text and adjacency indexes. A query opens that database read-only and performs bounded SQL traversal. It does not rebuild an in-memory graph or rescan project files before answering.

Updates compare source and configuration fingerprints, parse the changed inputs, and re-resolve the references affected by those changes. Graf publishes the replacement as one transaction, so readers continue to see the previous complete generation until the new one is ready.

The measured result combines native startup, persistent indexing, incremental updates, and the direct query path. This comparison did not isolate how much of the difference came from each change.

## The tradeoffs

The comparison was broad, not a universal win:

- Graf's cold index was 14.45% slower on clap and 6.30% slower on Zod.
- Zod used more peak memory in Graf for no-op, add, and delete updates.
- Graf's default database used 3.30x to 19.54x as much logical disk as Graphify's retained files across the measured stages. Graf keeps extra indexes and evidence to make later queries fast and precise.
- Graf sometimes returns an explicit ambiguity where Graphify chooses an answer. Those fast ambiguity exits received no correctness credit.
- The comparison covers these repositories, workloads, versions, and host. Different codebases and machines will produce different absolute times.

The broader comparison did not pass its strict all-observation qualification. Across 336 observations per product, Graf recorded 255 strict passes, 63 quality failures, and 18 execution or ambiguity outcomes; Graphify recorded 234 strict passes, 102 quality failures, and no execution failures. The 7x and 40x summaries exclude every pair where either product failed the predeclared checks. Those checks covered selected symbols, call edges, forbidden edges, additions, deletions, and query results; they did not prove complete semantic equivalence between the two graphs.

Graf 0.5.0 was compared with the frozen Graphify 0.9.65 release. The charts in the README round the geometric means for a memorable product-level summary; they do not claim every individual operation is 7x or 40x faster.
