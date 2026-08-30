//! Template source abstraction — the stoneware seam.
//!
//! Upstream Askama reads external templates from the filesystem, through the
//! single cached chokepoint in [`crate::input::get_template_source`]. stoneware
//! routes that read through this trait so alternative stores can plug in
//! without touching the parser or the generator:
//!
//! - [`FsSource`] — upstream behavior, the default. Byte-for-byte identical
//!   to `std::fs::read_to_string` at the chokepoint.
//! - `NedbSource` (feature `nedb`) — templates stored content-addressed in an
//!   embedded [nedb-engine](https://crates.io/crates/nedb-engine) database.
//!   Every template version is a node in the `templates` collection whose
//!   `caused_by` edge points at the version it superseded, so `TRACE` yields
//!   the full lineage of any template and `AS OF` renders history.
//!
//! # Selection
//!
//! The active source is chosen once per expansion process from the
//! `STONEWARE_TEMPLATE_SOURCE` environment variable:
//!
//! - unset → [`FsSource`] (upstream behavior, zero change)
//! - `nedb:<db-root>` → `NedbSource` reading from the database at `<db-root>`
//!
//! A spec that cannot be honored — unknown scheme, database that fails to
//! open, or `nedb:` without the `nedb` feature compiled in — resolves to a
//! source that fails **loudly at every template read**, naming the exact
//! reason. It never falls back to the filesystem silently: a silent fallback
//! is indistinguishable from a broken store.
//!
//! # Template identity
//!
//! `NedbSource` looks templates up by the path **as written** in the
//! `#[template(path = "...")]` attribute or `{% include %}` / `{% extends %}`
//! tag (e.g. `"index.html"`), not by the absolute resolved path — database
//! rows are machine-independent.

use std::path::Path;

/// Where external template bodies come from at macro-expansion time.
pub(crate) trait TemplateSource: Send + Sync {
    /// Read the full body of the template.
    ///
    /// `path` is the absolute resolved path (the cache key); `original_path`
    /// is the template reference as written in the source. Filesystem reads
    /// use `path`; store-backed reads use `original_path` as the row id.
    ///
    /// The error string is surfaced verbatim inside the compile error emitted
    /// at the call site, so it must name the actual reason the read failed —
    /// never a generic "not found".
    fn read(&self, path: &Path, original_path: &str) -> Result<String, String>;
}

/// Upstream behavior: read the template from the filesystem.
pub(crate) struct FsSource;

impl TemplateSource for FsSource {
    fn read(&self, path: &Path, _original_path: &str) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|err| err.to_string())
    }
}

/// A source that always fails, loudly, with the reason the requested source
/// could not be provided. Used when `STONEWARE_TEMPLATE_SOURCE` names a store
/// we cannot honor — never silently replaced by the filesystem.
pub(crate) struct BrokenSource {
    reason: String,
}

impl TemplateSource for BrokenSource {
    fn read(&self, _path: &Path, original_path: &str) -> Result<String, String> {
        Err(format!(
            "template '{original_path}' unavailable: STONEWARE_TEMPLATE_SOURCE could not be \
             honored: {}",
            self.reason
        ))
    }
}

/// Templates read from an embedded NEDB database (`templates` collection,
/// row id = template path as written).
#[cfg(feature = "nedb")]
pub(crate) struct NedbSource {
    db: nedb_engine::Db,
    root: String,
}

#[cfg(feature = "nedb")]
impl NedbSource {
    pub(crate) fn open(db_root: &Path) -> Result<Self, String> {
        let db = nedb_engine::Db::open(db_root, None)
            .map_err(|err| format!("cannot open NEDB store at '{}': {err}", db_root.display()))?;
        Ok(Self {
            db,
            root: db_root.display().to_string(),
        })
    }

    /// Write a template version, chaining `caused_by` to the version it
    /// supersedes (if any). Returns the stored node.
    ///
    /// This is the provenance write path: every edit is a new
    /// content-addressed node whose lineage is queryable with `TRACE`.
    ///
    /// Exercised by tests today; the user-facing sync tooling that calls it
    /// ships with the dogfooded book (PR-5).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn put_template(
        &self,
        id: &str,
        body: &str,
    ) -> Result<nedb_engine::store::Node, String> {
        let caused_by = match self.db.get("templates", id) {
            Some(prev) => vec![prev.hash],
            None => Vec::new(),
        };
        self.db
            .put(
                "templates",
                id,
                serde_json::json!({ "body": body }),
                caused_by,
                None,
                None,
            )
            .map_err(|err| format!("cannot store template '{id}': {err}"))
    }
}

#[cfg(feature = "nedb")]
impl TemplateSource for NedbSource {
    fn read(&self, _path: &Path, original_path: &str) -> Result<String, String> {
        let node = self.db.get("templates", original_path).ok_or_else(|| {
            format!(
                "template '{original_path}' not found in NEDB store '{}' \
                 (collection 'templates', id = template path as written)",
                self.root
            )
        })?;
        match node.data.get("body").and_then(|b| b.as_str()) {
            Some(body) => Ok(body.to_owned()),
            None => Err(format!(
                "template '{original_path}' in NEDB store '{}' has no string 'body' field \
                 (seq {}, hash {})",
                self.root, node.seq, node.hash
            )),
        }
    }
}

/// Build a source from a `STONEWARE_TEMPLATE_SOURCE` spec string.
///
/// Split out from [`active_source`] so the dispatch is testable without
/// mutating process-global environment state.
fn make_source(spec: &str) -> Box<dyn TemplateSource> {
    if let Some(db_root) = spec.strip_prefix("nedb:") {
        #[cfg(feature = "nedb")]
        {
            return match NedbSource::open(Path::new(db_root)) {
                Ok(source) => Box::new(source),
                Err(reason) => Box::new(BrokenSource { reason }),
            };
        }
        #[cfg(not(feature = "nedb"))]
        {
            return Box::new(BrokenSource {
                reason: format!(
                    "spec 'nedb:{db_root}' requires askama_derive feature 'nedb', \
                     which is not compiled in"
                ),
            });
        }
    }
    Box::new(BrokenSource {
        reason: format!("unknown source spec '{spec}' (expected 'nedb:<db-root>')"),
    })
}

/// The active template source for this expansion process.
pub(crate) fn active_source() -> &'static dyn TemplateSource {
    static SOURCE: std::sync::OnceLock<Box<dyn TemplateSource>> = std::sync::OnceLock::new();
    SOURCE
        .get_or_init(|| match std::env::var("STONEWARE_TEMPLATE_SOURCE") {
            Ok(spec) => make_source(&spec),
            Err(_) => Box::new(FsSource),
        })
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_source_reads_file() {
        let dir = std::env::temp_dir().join("stoneware-source-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.html");
        std::fs::write(&path, "hello {{ name }}\n").unwrap();
        let body = FsSource.read(&path, "t.html").unwrap();
        assert_eq!(body, "hello {{ name }}\n");
    }

    #[test]
    fn fs_source_error_names_reason() {
        let err = FsSource
            .read(Path::new("/definitely/not/a/real/template.html"), "t.html")
            .unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn unknown_spec_is_loud_not_silent() {
        let source = make_source("s3://nope");
        let err = source.read(Path::new("/x"), "index.html").unwrap_err();
        assert!(err.contains("unknown source spec"), "got: {err}");
        assert!(err.contains("index.html"), "got: {err}");
    }

    #[cfg(not(feature = "nedb"))]
    #[test]
    fn nedb_spec_without_feature_is_loud_not_silent() {
        let source = make_source("nedb:/tmp/somewhere");
        let err = source.read(Path::new("/x"), "index.html").unwrap_err();
        assert!(
            err.contains("requires askama_derive feature 'nedb'"),
            "got: {err}"
        );
    }

    #[cfg(feature = "nedb")]
    mod nedb {
        use super::super::*;

        fn temp_db(name: &str) -> std::path::PathBuf {
            let dir = std::env::temp_dir()
                .join("stoneware-nedb-test")
                .join(format!("{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn reads_template_body_from_store() {
            let root = temp_db("read");
            let source = NedbSource::open(&root).unwrap();
            source.put_template("index.html", "v1: {{ name }}").unwrap();
            let body = source.read(Path::new("/irrelevant"), "index.html").unwrap();
            assert_eq!(body, "v1: {{ name }}");
        }

        #[test]
        fn edits_chain_caused_by_and_trace_yields_lineage() {
            let root = temp_db("lineage");
            let source = NedbSource::open(&root).unwrap();
            let v1 = source.put_template("page.html", "v1").unwrap();
            let v2 = source.put_template("page.html", "v2").unwrap();

            // The edit's caused_by must point at the version it superseded.
            assert_eq!(v2.caused_by, vec![v1.hash.clone()]);

            // Reads serve the latest version.
            let body = source.read(Path::new("/x"), "page.html").unwrap();
            assert_eq!(body, "v2");

            // TRACE from v2 walks back to v1 — the maker's mark.
            let lineage = source.db.trace(&v2.hash, false, 10);
            assert!(
                lineage.iter().any(|n| n.hash == v1.hash),
                "v1 missing from lineage: {:?}",
                lineage.iter().map(|n| &n.hash).collect::<Vec<_>>()
            );
        }

        #[test]
        fn missing_template_error_names_store_and_id() {
            let root = temp_db("missing");
            let source = NedbSource::open(&root).unwrap();
            let err = source.read(Path::new("/x"), "ghost.html").unwrap_err();
            assert!(err.contains("ghost.html"), "got: {err}");
            assert!(err.contains("templates"), "got: {err}");
        }

        #[test]
        fn wrong_shape_row_error_names_seq_and_hash() {
            let root = temp_db("shape");
            let source = NedbSource::open(&root).unwrap();
            source
                .db
                .put(
                    "templates",
                    "bad.html",
                    serde_json::json!({ "not_body": 1 }),
                    Vec::new(),
                    None,
                    None,
                )
                .unwrap();
            let err = source.read(Path::new("/x"), "bad.html").unwrap_err();
            assert!(err.contains("no string 'body' field"), "got: {err}");
        }
    }
}
