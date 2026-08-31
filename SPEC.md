# stoneware — templates fired to stone

A surgical fork of [Askama](https://github.com/askama-rs/askama) with two features
upstream cannot ship as a wrapper, plus provenance dogfooded from inception.

> Clay in dev. Stone in prod. A maker's mark on every piece.

© Interchained LLC. Fork base: askama-rs/askama @ f98f8c5.

## Why fork

Two real gaps in the Rust templating niche, both requiring the derive internals:

1. **The compile-time / hot-reload false dichotomy.** Today you pick Askama's
   type safety OR MiniJinja-style reload-without-rebuild. Nobody ships both from
   one template source.
2. **Templates have no provenance.** A template is content — but no engine
   treats it as versioned, causally-linked data. Who changed this template, when,
   caused by what, and what did the page look like before? Unanswerable today.

## Pillar A — greenware mode (dev hot-reload)

Same template file, two backends:

- **`release` build (fired):** exactly upstream Askama. Templates compile into
  the binary through `askama_derive`. Full type checking, zero runtime cost.
  A typo in a field is a compile error.
- **`debug` build (greenware):** the derive additionally emits an interpreted
  fallback path that re-reads the template source at render time. Edit the
  template, refresh, see it — no rebuild. Struct fields are exposed to the
  interpreter through a generated context map, so the *data contract* stays the
  compile-checked one; only the template body is late-bound.

Feature-gated (`greenware`), default-on in debug, force-off with
`STONEWARE_FIRED=1` for debugging prod behavior locally. If interpretation
fails at render time in greenware mode the error names the template, the span,
and the reason — never a silent fallback to the compiled version, because a
silent fallback is indistinguishable from a broken reload.

## Pillar B — NEDB template source (provenance)

A new template source alongside the filesystem: embedded
[`nedb-engine`](https://crates.io/crates/nedb-engine) (Rust core, content-addressed
DAG store).

- Templates live in a `templates` collection: `{ path, body, _hash }`.
  Content-addressed — identical body, identical hash.
- Every template edit is a new write with `caused_by -> previous version hash`.
  `TRACE caused_by` = the full lineage of any template.
- `AS OF seq` rendering: render the site exactly as it stood at any point in
  history. Time-travel for pages.
- Optional render receipts: a `renders` collection recording
  `{ template_hash, data_hash, at }` with `caused_by -> template version` —
  a tamper-evident answer to "which template produced this page?"

The seam is narrow by design: `askama_derive/src/input.rs` reads template
sources through a single `read_to_string` call site (and `config.rs` for
configuration). Pillar B introduces a `TemplateSource` trait at that seam with
two impls: `FsSource` (upstream behavior, default) and `NedbSource`.

## Dogfood rule

stoneware's own book/examples render through stoneware with templates stored in
NEDB **from the first feature commit**. The repo's own template history is the
first public TRACE chain. We do not ship a provenance feature we do not run.

## Fork discipline

- Diff surface stays surgical: the `TemplateSource` seam, the greenware
  interpreter, and crate renames. Everything else tracks upstream.
- Upstream is merged regularly; divergence beyond the two pillars is a defect.
- Crates publish as `stoneware` / `stoneware-derive` (names verified free on
  crates.io 2026-08-30). No publish until Mark blesses the name.
- Never force-push, never rewrite public history, branch + PR only.

## Roadmap

- **PR-1 (this):** SPEC, fork baseline verified green (`cargo test --workspace`).
- **PR-2:** `TemplateSource` seam — trait + `FsSource`, zero behavior change,
  upstream suite stays green.
- **PR-3:** `NedbSource` + provenance writes + a worked example with TRACE.
- **PR-4:** greenware interpreter (debug-mode hot reload) behind the feature gate.
- **PR-5:** crate rename + dogfooded book + first publish (name blessed first).

Fragment-first rendering (the HTMX gap) is a known follow-up, deliberately out
of scope for v0.
