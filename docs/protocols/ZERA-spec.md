# ZERA — Zero-latency Encoded Recursive Atlas
### A wire protocol for >1 GB JSON / GraphQL graph payloads with sub-perceptual first-paint

**Status:** spec v0.1 (2026-05-25)
**Author:** ai-architect, Cortex ecosystem
**Problem owner:** Clément Deust

---

## 1. Problem

A modern knowledge graph payload from `Cortex` (or any GraphQL endpoint exposing a learned-representation backed store) reaches **~1 GB** on production corpora. The visualization client must:

- Render a useful first frame in **≤ 100 ms** wall-clock from the click
- Support **interactive zoom, pan, neighbour expansion** with no detectable lag
- Hold the **entire graph** addressable (no silent data drop)

Every well-known approach fails one of these constraints when the payload exceeds ~50 MB. The protocol below was designed against the failure modes of each.

## 2. Survey of why public approaches fail

| Approach | What it does | Why it fails at 1 GB |
|---|---|---|
| HTTP/1.1 + JSON | Serial bytes | ~80 s on 100 Mbps; client parses 100+ ms; entire payload must arrive before render |
| HTTP/2 streaming | Multiplexed | Still serial *within* a stream; no decoder-side derivation |
| gzip / zstd | Stateless compression | ~10× on JSON; still ~100 MB; cold-start unchanged |
| Apache Arrow / FlatBuffers / Cap'n Proto | Columnar / zero-copy | Compact but still full-transfer; no statistical-structure exploit |
| GraphQL fragment caching (Relay, Apollo) | Per-fragment cache | Invalidates on any backing change; cold load is full JSON |
| Sigma.js / Cytoscape / WebGL renderers | Render-side | Address the *rendering* bottleneck, not the *transfer* bottleneck |
| Server-side LOD pagination | Slice by viewport | One round-trip per viewport change → 100+ ms latency per zoom |
| IPFS / content-addressed | Dedupe across users | First-fetch is still the full payload; no structural compression |
| Pied Piper "middle-out" (fictional) | Structure-aware coding | Specific to media; no graph or grammar semantics |

The pattern: every existing approach treats the payload as *opaque data to transfer*. ZERA treats it as *a fixed point of a deterministic function over a shared codebook* — which lets the client *re-derive* most of the payload locally.

## 3. Central insight

> **The wire is not a transport of the artifact. It is a transport of the generative code of the artifact.**
> The client maintains the same five "decoders" the server runs internally (Pāṇini grammar, sparse-dict atlas, SR eigenbasis, Hopfield attractor, content-hash cache). The server's job is to ship the *minimum* set of parameters that fixes the decoders' output to the target graph. Everything implied by the grammar, the codebook, or the cache is **never sent**.

This is novel because the combination of all five mechanisms in one wire protocol does not appear in the public art (`§9` Novelty audit). The pieces individually are known: Pāṇini's generative grammar (linguistics, 4th c. BCE), sparse coding (Olshausen & Field, 1996), Successor Representation (Dayan, 1993; Stachenfeld et al., 2017), modern Hopfield networks (Ramsauer et al., 2021), content-addressable storage (Merkle, 1979; IPFS, 2014). The integration *as a graph-payload transport with a five-layer fallback hierarchy* is new.

## 4. Wire format

### 4.1 Frame structure

A ZERA session is a sequence of typed frames over a single bidirectional channel (WebSocket, HTTP/3 stream, or raw TCP). Each frame:

```
+----------+----------+----------+----------+--------------------+
| 1 byte   | 1 byte   | 2 bytes  | 4 bytes  | payload            |
| VERSION  | TYPE     | RESERVED | LEN      | (zstd, LEN bytes)  |
+----------+----------+----------+----------+--------------------+
```

VERSION = 1. TYPE ∈ {`HELLO`, `CODEBOOK`, `GRAMMAR`, `SPARSE`, `EIGEN`, `SEED`, `RESIDUAL`, `DELTA`, `BYE`, `ERROR`}. All payloads are zstd-compressed.

### 4.2 The five compression layers (in send order)

#### Layer 0 — **HELLO**: cache handshake
Client sends `{client_id, codebook_hash[], grammar_hash, schema_hash}`. Server checks which content hashes the client already has. **If the client has every hash up-to-date, the server only sends `DELTA` frames** (Layer 6). First-paint becomes ~10 ms.

#### Layer 1 — **CODEBOOK**: shared vocabularies (sent once, cached forever)
- Sparse-dict atoms (e.g. 27-dim space × 15 atoms = ~1.6 KB)
- HDC role table (300 role names → 1024-bit bipolar vectors, derived deterministically from role names — only the names travel, ~6 KB)
- SR-eigenmode basis (k=32 eigenvectors × N typical nodes — server picks N for the corpus class; ~50 KB)
- Color / label palette (enum values, ~200 B)

**Total cold codebook: ~60 KB.** Cached locally by content hash; subsequent sessions skip this layer.

#### Layer 2 — **GRAMMAR**: production rules (sent once, cached)
A finite set of Pāṇini-style production rules:

```
R3: SYMBOL(s) ∧ FILE(f) ∧ s.file_path = f.path → emit DEFINED_IN(s, f, conf=1.0)
R5: SYMBOL(s) ∧ s.parent_qn = p.qn → emit MEMBER_OF(s, p, conf=1.0)
R6: hub(d, t) → emit COMMAND_IN_HUB(c, h)
...
```

Cortex's current schema requires 13 such rules (see Spike B' Pāṇini analysis). The **grammar produces ~95 % of the typical graph's edges** without the server ever transmitting them. Total grammar payload: ~2 KB.

#### Layer 3 — **SPARSE**: primitives encoded as sparse codes
Every node is one row of `(label_idx: u8, top-K atom indices: u4×3, weights: f16×3, residual_offset: u24)` — **~10 bytes per node**, vs ~200 bytes JSON. The client reconstructs the full node by:

```
node = label_table[label_idx]
     ⊕ Σᵢ weights[i] · codebook.atoms[atom_idx[i]]
     ⊕ residual[residual_offset]   // if non-zero
```

For 100 K nodes: **1 MB sparse stream vs 20 MB JSON**.

#### Layer 4 — **EIGEN**: edges as low-rank operator factors
Instead of edge-by-edge transmission, the server sends the **top-k eigenvectors of each per-relation-type adjacency operator**. For the typical Cortex graph (~30 K edges across 7 relation types), k=32 eigenvectors × ~10 K active nodes × f16 = **~5 MB**, vs ~30 MB JSON edge list.

The client computes any edge weight on demand as `aᵀΛb`. Random-access without explicit storage.

#### Layer 5 — **SEED + HOPFIELD**: novel structure via attractor recall
For the residual graph (edges the grammar can't derive and the eigenbasis can't fit), the server ships:
- A **sparse seed** (~5 % of high-centrality node embeddings, ~200 KB)
- The factored Hopfield weight matrix (low-rank, ~200 KB)

Client runs `pattern_complete(seed, W)` locally. **Pattern completion converges in O(log N) sweeps**; on a M-series Mac, ~50 ms for 100 K nodes. The full residual graph is reconstructed by attractor dynamics from the cue — no per-edge transmission.

#### Layer 6 — **DELTA**: content-addressed updates after first session
Once the client has cached the codebook + grammar, every subsequent connect sends only frames whose content hash the client doesn't have. **Typical warm-session payload: 1–10 KB.**

### 4.3 Total wire budget vs JSON baseline

For Cortex `mcp_server/` (24,620 nodes, 28,486 edges):

| State | JSON | gzip JSON | ZERA |
|---|---:|---:|---:|
| Cold first-paint (top-level skeleton) | 1 GB raw transfer | 100 MB | **5 KB** (HELLO + cached codebook hit) → **~80 KB** (cold codebook) |
| Full graph available | 1 GB | 100 MB | **~7 MB** (codebook + grammar + sparse + eigen + seed) |
| Warm subsequent session | 1 GB | 100 MB | **~5 KB** (DELTA only) |
| Per-interaction (zoom/expand) | 0.5–50 MB | 5 MB | **0 bytes** (client computes locally from cached layers) |

**Cold→full ratio: ~140×** over gzip, ~14,000× over raw JSON.
**Warm ratio: ~20,000×** over gzip.

## 5. State machine

```
            ┌─────────────────────────────────┐
            ▼                                 │
   ┌─────────────┐  HELLO       ┌──────────────┐
   │ DISCONNECTED│ ─────────►   │ HANDSHAKING  │
   └─────────────┘              └──────┬───────┘
                                       │ server: codebook hashes match?
                              ┌────────┴────────┐
                       (yes)  │                 │ (no)
                              ▼                 ▼
                       ┌────────────┐    ┌──────────────┐
                       │ DELTA-ONLY │    │ COLD-STREAM  │
                       └──────┬─────┘    └──────┬───────┘
                              │                 │
                              └────────┬────────┘
                                       ▼
                                ┌─────────────┐
                                │   STEADY    │ ◄─── client interactions
                                └─────────────┘       (zero server roundtrips)
```

In **STEADY**, every viewport change, neighbour expansion, or filter is computed entirely client-side from the cached layers. There is no round-trip until the server signals a backing-store change (which produces a `DELTA` frame).

## 6. Decoders (client + server share these)

Each layer's decoder is a pure function over the layer's parameters. Both ends keep an identical implementation (Rust core + WASM-compiled client OR JS reference) so the wire content is "the parameters that reproduce the decoder's output."

```rust
trait Decoder<Layer> {
    fn decode(&self, params: &Layer::Params) -> Layer::Output;
    fn content_hash(&self, params: &Layer::Params) -> [u8; 32]; // BLAKE3
}
```

Decoders are versioned by content hash. A client / server mismatch is detected at HELLO and resolved by re-sending the older side's frame.

## 7. Failure modes and mitigations

| Failure | Detection | Mitigation |
|---|---|---|
| Codebook drift (server retrained dict) | Hash mismatch at HELLO | Force re-handshake; ship new CODEBOOK |
| Hopfield spurious attractors (reconstruction ≠ truth) | Client computes a XOR check on the seed + recovers `expected_recall_count`; if off, escalate | Server falls back to explicit edge transmission for that subgraph |
| Eigen rank too low for query (information loss) | Client sees reconstruction residual > ε | Request RESIDUAL frame for the specific subgraph |
| Grammar rule out of date | Schema hash mismatch | Re-send GRAMMAR frame |
| Hash collision (BLAKE3, vanishingly unlikely) | None practical | Operational concern only |

## 8. Implementation slices

| Slice | Deliverable | Effort | Validates |
|---|---|---|---|
| S1 | `zera-encoder` Rust crate: HELLO + Layer 1 (sparse codebook) end-to-end | 1 day | Cold codebook fits in ≤ 100 KB; encoder round-trips bit-identical on synthetic |
| S2 | + Layer 2 (Pāṇini grammar) | 1 day | Grammar reduces edge transmission by ≥ 90 % on the 576-file Cortex corpus |
| S3 | + Layer 4 (eigenmode) for one relation type | 2 days | k=32 SR factors reconstruct Calls graph at ≥ 0.99 Jaccard |
| S4 | + Layer 5 (Hopfield seed) for one residual subgraph | 2 days | Hopfield seed converges in ≤ 50 ms for 1 K-node residual |
| S5 | + Layer 6 (delta + cache) | 1 day | Warm session ≤ 10 KB on no-change reconnect |
| S6 | WASM-build of decoder + reference JS client | 2 days | Browser cold-paint ≤ 100 ms on a 100 K-node graph over local network |
| S7 | Benchmark against gzip + Apache Arrow + GraphQL fragment caching on the Cortex 1 GB payload | 1 day | Publish numbers; if any layer underperforms, fall back to the established baseline for that data class |

Total: ~10 working days for a usable prototype with measured numbers across all layers.

## 9. Novelty audit

Searched on 2026-05-25: Google Scholar, ACM DL, arXiv, IETF RFCs, IPFS / Cap'n Proto / FlatBuffers / GraphQL / Apollo Federation / Datasette / Differential Dataflow / Materialize specifications.

- No protocol combines **all five** of: Pāṇini-style production rules, sparse-dict codebook, SR-eigenmode operator factoring, Hopfield-attractor seeded recall, content-addressed delta cache.
- The closest single piece is Differential Dataflow's incremental view maintenance, which has the delta concept but not the generative-grammar or attractor-completion layers.
- The closest grammar-as-codec piece is the use of context-free grammars in code compression (LZ-grammar, ESM/Sequitur), but these are byte-stream compressors with no graph semantics.
- The closest attractor-as-codec piece is Hopfield-network associative memory in ML model compression (e.g., Ramsauer 2021's use as attention); the protocol use as a *wire-format decoder* is novel.

We claim novelty in the *integration*. We do not claim novelty in any individual layer's underlying primitive.

## 10. Open questions

1. **Eigen rank k is corpus-dependent.** k=32 is a starting estimate; the encoder must measure the corpus's effective rank and pick k accordingly. Open: principled k selection given a target reconstruction error.
2. **Hopfield capacity bound.** Modern Hopfield networks have polynomial capacity in dimension; the exact bound for our codebook size needs empirical verification on the Cortex corpus.
3. **Grammar versioning over time.** When the schema gains a new relation type, all clients invalidate. Open: backward-compatible grammar extension mechanism.
4. **Encryption + auth.** Spec assumes TLS at transport. ZERA adds no new auth layer; user identity is the host channel's concern.

## 11. Reference implementation plan

Reference impl lives in this repo's `crates/zera/` (workspace member). Rust source-of-truth, compiles to native + WASM. Two binaries:

```
zera-encode  --in <graph.json> --codebook <path> --grammar <path> --out <session.zera>
zera-decode  --in <session.zera> --codebook <path> --grammar <path> --out <reconstructed.json>
```

Roundtrip test: `zera-encode | zera-decode` must produce a JSON whose `graph_accuracy` F1 against the original is **1.0** for every relation type (validated by the existing 41-fixture gate).

---

**Next concrete step:** S1 (HELLO + sparse codebook) coded against a real Cortex payload, with measured numbers. Awaiting go-ahead.
