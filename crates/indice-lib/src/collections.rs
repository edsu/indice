use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SeedPage {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub ts: String,
}

/// Where a collection's WACZ lives: a local file, a remote http(s) URL, or a
/// resource in a Browsertrix instance that must be re-resolved to a fresh
/// presigned URL on demand (see [`Source::Browsertrix`]).
///
/// Serializes as a plain string (the path/URL, or `browsertrix|…`) for a
/// readable manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Source {
    File(PathBuf),
    Url(String),
    /// A WACZ resource inside a Browsertrix instance, identified stably by
    /// `host` + `org` + archived-item `item` + `resource` filename. Browsertrix
    /// serves WACZs via **presigned URLs that expire (~48 h)**, so we store the
    /// identity, not a URL, and re-resolve a fresh presigned URL each time we
    /// index or replay it (via a [`crate::index::SourceResolver`], which holds
    /// the credentials). Stored as `browsertrix|host|org|item|resource`.
    Browsertrix {
        host: String,
        org: String,
        item: String,
        resource: String,
    },
    /// A WACZ resource inside a **public** Browsertrix collection, re-resolvable
    /// without credentials. Public presigned URLs also expire, but the fresh URL
    /// comes from the *collection*-scoped public `replay.json`
    /// (`/api/orgs/{org}/collections/{collection}/public/replay.json`), so we
    /// store the collection id rather than an archived-item id. Stored as
    /// `browsertrix-public|host|org|collection|resource`.
    BrowsertrixPublic {
        host: String,
        org: String,
        collection: String,
        resource: String,
    },
}

impl Source {
    /// Parse a location string: `browsertrix|…` is a Browsertrix resource,
    /// `http://`/`https://` a URL, else a file path.
    pub fn parse(s: &str) -> Self {
        // Check the more specific `browsertrix-public|` prefix first.
        if let Some(rest) = s.strip_prefix("browsertrix-public|") {
            let p: Vec<&str> = rest.splitn(4, '|').collect();
            if p.len() == 4 {
                return Source::BrowsertrixPublic {
                    host: p[0].to_string(),
                    org: p[1].to_string(),
                    collection: p[2].to_string(),
                    resource: p[3].to_string(),
                };
            }
        }
        if let Some(rest) = s.strip_prefix("browsertrix|") {
            // host|org|item|resource — split into exactly 4; the resource is the
            // remainder, so a `|` in a filename (unheard of from Browsertrix)
            // lands harmlessly in the last field.
            let p: Vec<&str> = rest.splitn(4, '|').collect();
            if p.len() == 4 {
                return Source::Browsertrix {
                    host: p[0].to_string(),
                    org: p[1].to_string(),
                    item: p[2].to_string(),
                    resource: p[3].to_string(),
                };
            }
        }
        if s.starts_with("http://") || s.starts_with("https://") {
            Source::Url(s.to_string())
        } else {
            Source::File(PathBuf::from(s))
        }
    }

    pub fn is_url(&self) -> bool {
        matches!(self, Source::Url(_))
    }

    /// Whether replaying/verifying this source needs a live fetch rather than a
    /// local file (a URL or a Browsertrix resource).
    pub fn is_remote(&self) -> bool {
        !matches!(self, Source::File(_))
    }

    /// The local file path, if this is a file source.
    pub fn as_file(&self) -> Option<&Path> {
        match self {
            Source::File(p) => Some(p.as_path()),
            Source::Url(_) | Source::Browsertrix { .. } | Source::BrowsertrixPublic { .. } => None,
        }
    }

    /// Stable string form: the file path, the URL, or `browsertrix|…`.
    pub fn location(&self) -> String {
        match self {
            Source::File(p) => p.to_string_lossy().into_owned(),
            Source::Url(u) => u.clone(),
            Source::Browsertrix {
                host,
                org,
                item,
                resource,
            } => format!("browsertrix|{host}|{org}|{item}|{resource}"),
            Source::BrowsertrixPublic {
                host,
                org,
                collection,
                resource,
            } => format!("browsertrix-public|{host}|{org}|{collection}|{resource}"),
        }
    }

    /// Build a File source for an absolute path, stored relative to `home` when
    /// the path is under it (so the home folder is portable), else absolute.
    pub fn for_file(abs: &Path, home: &Path) -> Source {
        let home_abs = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
        match abs.strip_prefix(&home_abs) {
            Ok(rel) => Source::File(rel.to_path_buf()),
            Err(_) => Source::File(abs.to_path_buf()),
        }
    }

    /// Resolve a File source to a concrete path against `home`: relative paths
    /// are joined to `home`, absolute paths are returned as-is. `None` for URLs.
    pub fn resolve(&self, home: &Path) -> Option<PathBuf> {
        match self {
            Source::File(p) if p.is_absolute() => Some(p.clone()),
            Source::File(p) => Some(home.join(p)),
            Source::Url(_) | Source::Browsertrix { .. } | Source::BrowsertrixPublic { .. } => None,
        }
    }
}

impl From<String> for Source {
    fn from(s: String) -> Self {
        Source::parse(&s)
    }
}

impl From<Source> for String {
    fn from(s: Source) -> Self {
        s.location()
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.location())
    }
}

/// A single WACZ file in the archive - one member of a [`Collection`]. (This was
/// previously the top-level `Collection`; a curated `Collection` now groups many
/// of these.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wacz {
    pub id: String,
    /// Id (slug) of the [`Collection`] this WACZ belongs to. Every crawl belongs
    /// to a collection — supplied at index/import time, held here in the manifest
    /// (the authoritative membership record, incl. for remote sources with no
    /// local file).
    #[serde(default, deserialize_with = "lenient_collection")]
    pub collection: CollectionId,
    /// The WACZ location. Older manifests used the key `path`.
    #[serde(alias = "path")]
    pub source: Source,
    pub name: String,
    pub date_indexed: String,
    pub file_size: u64,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crawl_date: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seed_pages: Vec<SeedPage>,

    // ── Provenance (from datapackage.json + the WARC warcinfo record) ──
    /// Software that produced this archive, as reported by the WACZ
    /// `datapackage.json` and/or the WARC `warcinfo` record (e.g.
    /// `Browsertrix-Crawler 1.13.0`, `py-wacz 0.4.6`). We do not try to label
    /// which entry crawled vs packaged - the formats don't distinguish - so this
    /// is just the set of tools involved, joined for display at the UI level.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "string_or_seq"
    )]
    pub software: Vec<String>,
    /// Contact for the operator who ran the crawl.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// User-Agent the crawler sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// How the crawler handled robots.txt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub robots: Option<String>,
    /// Number of pages indexed from this WACZ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u64>,
    /// Earliest capture timestamp seen (14-digit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_start: Option<String>,
    /// Latest capture timestamp seen (14-digit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_end: Option<String>,

    /// Where this WACZ was imported from, when it came via `indice
    /// browsertrix`. Drives incremental re-sync (skip already-imported items)
    /// and attributes provenance. Absent for hand-indexed WACZs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browsertrix: Option<BrowsertrixRef>,
    /// Where this WACZ came from, when built by `indice import archive-it` from a
    /// crawl's WARC files. Drives incremental re-sync (skip already-imported
    /// crawls) and attributes provenance. Absent for other WACZs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_it: Option<ArchiveItRef>,
    /// For a nested multi-WACZ (a WACZ bundling other WACZs), the number of inner
    /// WACZs flattened into this crawl. `None` for an ordinary (flat) WACZ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_waczs: Option<u64>,

    // ── Provenance fields previously parsed-but-dropped, or newly read (populate
    //    on reindex; all conditional so un-reindexed crawls just show less) ──
    /// WACZ last-modified time (datapackage `modified`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// Collection/crawl this WARC declares membership in (warcinfo `isPartOf`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_part_of: Option<String>,
    /// Host the crawl ran on (warcinfo `hostname`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// WARC spec the crawl conforms to (warcinfo `conformsTo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conforms_to: Option<String>,
    /// Topical keywords (datapackage `keywords`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// License labels (datapackage `licenses`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub licenses: Vec<String>,
    /// HTTP status-code histogram across all captures (from the CDX) — the
    /// derived "capture quality" / DACS Appraisal signal. Empty until reindex.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub status_counts: BTreeMap<u16, u64>,

    // ── Custody ──
    /// Who accessioned this crawl into the archive, when that is known.
    ///
    /// `None` for crawls indexed from the CLI or by a version before this
    /// existed: we genuinely don't know, and inventing an identity would be
    /// worse than admitting it. Authorization treats unattributed crawls as
    /// nobody's, so only an admin may deaccession them.
    ///
    /// Stores the [`SubjectId`](crate::identity::SubjectId), never a display
    /// name — the crawl page is public, and storing the name would recreate
    /// the login-address leak that sanitizing annotations fixed, in a new
    /// place. Names are resolved at render time through `users.yaml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<crate::identity::SubjectId>,
}

impl Wacz {
    /// Whether the WACZ is available, resolving file paths against `home`.
    /// Local files must exist on disk; remote URLs are assumed present.
    pub fn is_present(&self, home: &Path) -> bool {
        match self.source.resolve(home) {
            Some(path) => path.exists(),
            None => true, // URL source
        }
    }
}

/// Provenance for a WACZ imported from a Browsertrix instance (`indice
/// browsertrix`). The `(host, item_id, resource_hash)` triple lets a re-run skip
/// an item that's already indexed without re-downloading it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowsertrixRef {
    /// The Browsertrix host the item came from.
    pub host: String,
    /// The Browsertrix archived-item id (a crawl or an upload).
    pub item_id: String,
    /// The WACZ resource content hash from `replay.json` (e.g. `sha256:…`), when
    /// present — the strongest signal that content is unchanged.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resource_hash: String,
    /// Browsertrix QA review rating (1–5, Excellent→Bad), if a human reviewed the
    /// crawl. A DACS Appraisal signal surfaced on the crawl page. `None` if
    /// unreviewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_status: Option<u8>,
}

/// Provenance for a WACZ built from an Archive-It crawl's WARC files (`indice
/// import archive-it`). The `(host, collection_id, crawl_id)` triple lets a
/// re-run skip a crawl that's already imported without re-downloading it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveItRef {
    /// The Archive-It host the crawl came from.
    pub host: String,
    /// The Archive-It collection id.
    pub collection_id: i64,
    /// The Archive-It crawl (job) id whose WARCs this WACZ bundles.
    pub crawl_id: i64,
    /// How many WARC files from the crawl were packaged (a coarse change signal:
    /// if a crawl later grows more WARCs, `--force` re-imports it).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub warc_count: u64,
    /// The Archive-It collection's display title (e.g. "Stephen Ratcliffe
    /// Papers"), so the crawl page can name it — far more descriptive than the
    /// indice collection the crawl was imported into. Empty when uncollected.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub collection_title: String,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A curated collection: a named group of [`Wacz`] members with its own
/// curatorial (finding-aid) metadata. Aggregates (member count, size, capture
/// range, software) are derived from members at read time, not stored here.
///
/// The descriptive metadata is the *source of truth* in a git-committable
/// Markdown finding aid at `<home>/collections/<slug>/README.md` — YAML front-matter for
/// the short structured fields, and a Markdown body for the `narrative` (Scope
/// & Content / Custodial history / Appraisal prose). See [`load_finding_aids`]
/// / [`write_finding_aid`]. Fields are framed against DACS / EAD.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Collection {
    pub id: CollectionId,
    pub name: String,
    /// A short abstract / caption (EAD `<abstract>`), distinct from the longer
    /// `narrative`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When the collection was first created (RFC 3339).
    pub created: String,
    /// Who created it, and who last edited its finding aid, when known.
    /// `None` for collections created from the CLI or before custody was
    /// recorded. Stores the [`SubjectId`](crate::identity::SubjectId) — the
    /// collection page is public, so display names are resolved through
    /// `users.yaml` at render time rather than baked in here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<crate::identity::SubjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<crate::identity::SubjectId>,
    /// Who runs this indice instance / holds the collection (EAD
    /// `<repository>`), distinct from `creator`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curator: Option<String>,

    // ── Finding-aid front-matter (DACS / EAD) ──
    /// Collecting org/person responsible for the records (EAD `<origination>`,
    /// DACS Name of Creator) — distinct from `curator`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    /// Curatorial coverage statement (EAD `<unitdate>`, DACS Date), distinct
    /// from the auto-derived capture range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dates: Option<String>,
    /// Conditions governing access and use (EAD `<accessrestrict>` +
    /// `<userestrict>`, DACS 4.1/4.4) — one field labelled to cover both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rights: Option<String>,
    /// Topical access points (EAD `<controlaccess>`, DACS Subject).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<String>,

    // ── Finding-aid body ──
    /// The Markdown narrative: Scope & Content plus Custodial history /
    /// Appraisal, written as the curator sees fit (EAD `<scopecontent>` /
    /// `<custodhist>` / `<appraisal>`). Stored as the Markdown body of the
    /// finding-aid file, not in front-matter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrative: Option<String>,
}

/// The on-disk manifest. WACZ members (membership + derived provenance) live in
/// the *derived* `<home>/index/waczs.json`; collection descriptive metadata is
/// the *source of truth* in git-committable Markdown finding aids at
/// `<home>/collections/<slug>/README.md` (see [`load_finding_aids`]). A legacy
/// `index/collections.json` is read once and migrated to `.md` on the next save.
pub struct Manifest {
    index_dir: PathBuf,
    /// Rustyweb home (`index_dir`'s parent); holds `collections/` + `crawls/`.
    home: PathBuf,
    pub collections: Vec<Collection>,
    pub waczs: Vec<Wacz>,
    /// Collection ids whose finding aid needs (re)writing on `save` — the set
    /// created/modified this session, or migrated from legacy JSON. Untouched
    /// finding aids are never rewritten, so hand edits keep their formatting.
    dirty: HashSet<CollectionId>,
}

impl Manifest {
    pub fn open(index_dir: &Path) -> Result<Self> {
        let home = index_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| index_dir.to_path_buf());
        let collections_path = index_dir.join("collections.json");
        let waczs_path = index_dir.join("waczs.json");
        let collections_dir = home.join("collections");
        let mut dirty = HashSet::new();

        // Migrate an earlier flat layout (`collections/<slug>.md`) into the
        // per-collection dir form (`collections/<slug>/README.md`), preserving
        // curator-authored prose. Best-effort; harmless once already migrated.
        migrate_flat_finding_aids(&collections_dir);

        // ── WACZ members (derived index) ──
        let waczs: Vec<Wacz> = if waczs_path.exists() {
            read_json(&waczs_path)?.unwrap_or_default()
        } else if collections_path.exists() && legacy_json_holds_waczs(&collections_path)? {
            // Oldest layout: `collections.json` held the WACZ records directly.
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&collections_path)?)?;
            serde_json::from_value(value)?
        } else {
            Vec::new()
        };

        // Heal records that carry no usable collection — the oldest layout had
        // no `collection` key at all, and a manifest written mid-migration could
        // hold an empty one. Both arrive as the sentinel; the WACZ id doubles as
        // its collection id, matching the singleton collections synthesized
        // below. Applies to every load path, so a `waczs.json` in that state is
        // repaired rather than left orphaned.
        let mut waczs = waczs;
        for w in &mut waczs {
            if w.collection == CollectionId::default() {
                w.collection =
                    CollectionId::parse(&w.id).unwrap_or_else(|| CollectionId::from_name(&w.id));
            }
        }

        // ── Collection descriptive metadata (finding aids, source of truth) ──
        let collections: Vec<Collection> = if dir_has_findingaid(&collections_dir) {
            load_finding_aids(&collections_dir)?
        } else if collections_path.exists() {
            // Migrate from legacy `collections.json`, then write `.md` on save.
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&collections_path)?)?;
            let cols: Vec<Collection> = if legacy_json_holds_waczs(&collections_path)? {
                // Synthesize a singleton collection per legacy WACZ.
                waczs
                    .iter()
                    .map(|w| Collection {
                        id: CollectionId::parse(&w.id)
                            .unwrap_or_else(|| CollectionId::from_name(&w.id)),
                        name: w.name.clone(),
                        description: w.description.clone(),
                        created: w.date_indexed.clone(),
                        ..Default::default()
                    })
                    .collect()
            } else {
                // Per-element, so one malformed record can't silently discard
                // every curator's description/narrative (mirrors the
                // skip-and-warn in `load_finding_aids`).
                serde_json::from_value::<Vec<serde_json::Value>>(value)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|v| match serde_json::from_value::<Collection>(v.clone()) {
                        Ok(c) => Some(c),
                        Err(e) => {
                            tracing::warn!(record = %v, "skipping unreadable collection record: {e}");
                            None
                        }
                    })
                    .collect()
            };
            for c in &cols {
                dirty.insert(c.id.clone());
            }
            cols
        } else {
            Vec::new()
        };

        Ok(Self {
            index_dir: index_dir.to_path_buf(),
            home,
            collections,
            waczs,
            dirty,
        })
    }

    /// Open the manifest, apply `f`, and save, as one named operation.
    ///
    /// Every mutation of the manifest is a read-modify-write: [`save`] rewrites
    /// `waczs.json` wholesale from the in-memory vec, so an open, a change, and
    /// a save are a unit even though nothing about three separate calls says
    /// so. Naming it means a caller cannot mutate and forget to persist, and
    /// gives the invariant one place to be written down.
    ///
    /// `f` returning an error leaves the file untouched.
    ///
    /// # This does not serialize anything
    ///
    /// It is packaging, not a lock. Two callers running this concurrently will
    /// each read, change their own copy, and atomically install it, and the
    /// later save wins: the earlier one's changes are gone. Anything that can
    /// run concurrently must still hold whatever lock serializes it (in the
    /// server, `AppState::write_lock`). Doing attribution outside that lock is
    /// how a crawl's manifest entry got erased while its documents stayed in
    /// the index, which is the bug this doc exists to stop repeating.
    pub fn update<T>(index_dir: &Path, f: impl FnOnce(&mut Manifest) -> Result<T>) -> Result<T> {
        let mut manifest = Manifest::open(index_dir)?;
        let out = f(&mut manifest)?;
        manifest.save()?;
        Ok(out)
    }

    /// Insert or replace a WACZ member by id.
    pub fn upsert_wacz(&mut self, wacz: Wacz) {
        if let Some(pos) = self.waczs.iter().position(|w| w.id == wacz.id) {
            self.waczs[pos] = wacz;
        } else {
            self.waczs.push(wacz);
        }
    }

    /// Ensure a collection with `id` exists, creating a default one (named
    /// `name`) if it doesn't. Returns the collection id for convenience.
    pub fn ensure_collection(&mut self, id: &CollectionId, name: &str, created: &str) -> String {
        if !self.collections.iter().any(|c| &c.id == id) {
            self.collections.push(Collection {
                id: id.clone(),
                name: name.to_string(),
                created: created.to_string(),
                ..Default::default()
            });
            self.dirty.insert(id.clone());
        }
        id.to_string()
    }

    /// Create or update a collection's curatorial metadata by name (its id is
    /// the slug of the name). Only fields set in `fields` change; `created` is
    /// set on first creation. Merge policy is "fill gaps, curator wins": the
    /// caller decides what to pass (the CLI passes what the curator typed; an
    /// importer passes only fields that are still empty). Returns the id.
    /// `actor` is who is making the edit, when known — `None` from the CLI,
    /// which has no request identity. `created_by` is set only on creation so
    /// it stays the accession record; `updated_by` tracks the last editor.
    pub fn apply_fields(
        &mut self,
        name: &str,
        fields: &CollectionFields,
        created: &str,
        actor: Option<&crate::identity::SubjectId>,
    ) -> String {
        let id = CollectionId::from_name(name);
        self.dirty.insert(id.clone());
        if let Some(c) = self.collections.iter_mut().find(|c| c.id == id) {
            c.name = name.to_string();
            fields.apply_to(c);
            // Assigned unconditionally, so a CLI edit (`actor: None`) CLEARS a
            // previous web editor rather than leaving them named as the last
            // one — a stale attribution is worse than an absent one.
            c.updated_by = actor.cloned();
        } else {
            let mut c = Collection {
                id: id.clone(),
                name: name.to_string(),
                created: created.to_string(),
                created_by: actor.cloned(),
                updated_by: actor.cloned(),
                ..Default::default()
            };
            fields.apply_to(&mut c);
            self.collections.push(c);
        }
        id.to_string()
    }

    /// Auto-*seed* a collection's curatorial metadata from ingest (the WACZ
    /// datapackage, the Browsertrix API): like [`apply_fields`], but each value
    /// is applied only where the collection's field is still empty, so a curator's
    /// edit (or an earlier seed) is never overwritten. The collection is keyed by
    /// its stable `id` (not the display `name`, which a curator may edit — keying
    /// on the name would spawn a phantom collection after a rename); `name`/
    /// `created` are used only if it must be created. The finding aid is marked
    /// for rewrite **only when a field actually changed**, so re-indexing an
    /// already-seeded collection leaves its file (and any hand formatting) alone.
    ///
    /// [`apply_fields`]: Self::apply_fields
    pub fn seed_fields(
        &mut self,
        id: &CollectionId,
        name: &str,
        fields: &CollectionFields,
        created: &str,
    ) {
        if let Some(c) = self.collections.iter_mut().find(|c| &c.id == id) {
            if fields.apply_to_empty(c) {
                self.dirty.insert(id.clone());
            }
        } else {
            let mut c = Collection {
                id: id.clone(),
                name: name.to_string(),
                created: created.to_string(),
                ..Default::default()
            };
            fields.apply_to_empty(&mut c);
            self.collections.push(c);
            self.dirty.insert(id.clone());
        }
    }

    /// Persist the manifest: the derived `waczs.json`, plus a Markdown finding
    /// aid for every collection created/modified this session. Untouched finding
    /// aids are left on disk as-is (so hand edits keep their formatting).
    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.index_dir)?;
        // Atomically: this is the only record of which crawls exist, their
        // collections, and provenance set out of band (import refs, custody)
        // that a reindex cannot rebuild. A truncate-then-write that loses power
        // half way loses the archive's index of itself.
        crate::fsio::write_atomic_str(
            &self.index_dir.join("waczs.json"),
            &serde_json::to_string_pretty(&self.waczs)?,
        )?;
        for c in &self.collections {
            if self.dirty.contains(&c.id) {
                write_finding_aid(&self.home, c)?;
            }
        }
        Ok(())
    }

    pub fn wacz_by_id(&self, id: &str) -> Option<&Wacz> {
        self.waczs.iter().find(|w| w.id == id)
    }

    pub fn collection_by_id(&self, id: &str) -> Option<&Collection> {
        self.collections.iter().find(|c| c.id == id)
    }

    /// The WACZ members of a collection.
    pub fn members_of<'a>(&'a self, collection_id: &'a str) -> impl Iterator<Item = &'a Wacz> {
        self.waczs
            .iter()
            .filter(move |w| w.collection == collection_id)
    }

    /// Remove the WACZ entry with `id`, returning it if present. Touches only the
    /// manifest — not the index, the WACZ file, or thumbnails.
    pub fn remove_wacz(&mut self, id: &str) -> Option<Wacz> {
        self.waczs
            .iter()
            .position(|w| w.id == id)
            .map(|i| self.waczs.remove(i))
    }

    /// Remove the collection grouping with `id`, returning it if present. Members
    /// (WACZs) are not touched; the caller decides their fate.
    pub fn remove_collection(&mut self, id: &str) -> Option<Collection> {
        self.collections
            .iter()
            .position(|c| c.id == id)
            .map(|i| self.collections.remove(i))
    }
}

/// Read and parse a JSON file if it exists (`None` when absent).
fn read_json<T: for<'de> serde::Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&data)?))
}

// ── Finding-aid files (git-committable curator source of truth) ────────────────

/// A partial update to a collection's curatorial metadata. Every field is
/// optional so callers change only what they mean to; `subjects` is
/// `Option<Vec>` so "not provided" (`None`) differs from "clear to empty"
/// (`Some(vec![])`). Shared by the `collection set` CLI and the importer, with a
/// "fill gaps, curator wins" policy applied by the *caller* (see
/// [`Manifest::apply_fields`]).
#[derive(Debug, Clone, Default)]
pub struct CollectionFields {
    pub description: Option<String>,
    pub curator: Option<String>,
    pub creator: Option<String>,
    pub dates: Option<String>,
    pub rights: Option<String>,
    pub subjects: Option<Vec<String>>,
    pub narrative: Option<String>,
}

impl CollectionFields {
    /// Whether nothing is set (nothing to apply).
    pub fn is_empty(&self) -> bool {
        self.description.is_none()
            && self.curator.is_none()
            && self.creator.is_none()
            && self.dates.is_none()
            && self.rights.is_none()
            && self.subjects.is_none()
            && self.narrative.is_none()
    }

    /// Overwrite `c`'s fields for each value that is `Some` (leave the rest).
    fn apply_to(&self, c: &mut Collection) {
        if self.description.is_some() {
            c.description = self.description.clone();
        }
        if self.curator.is_some() {
            c.curator = self.curator.clone();
        }
        if self.creator.is_some() {
            c.creator = self.creator.clone();
        }
        if self.dates.is_some() {
            c.dates = self.dates.clone();
        }
        if self.rights.is_some() {
            c.rights = self.rights.clone();
        }
        if let Some(s) = &self.subjects {
            c.subjects = s.clone();
        }
        if self.narrative.is_some() {
            c.narrative = self.narrative.clone();
        }
    }

    /// Apply each `Some` value only where `c`'s corresponding field is *empty*
    /// (`None` / empty `Vec` / blank narrative). Used to auto-*seed* a collection
    /// from ingest metadata (WACZ datapackage, Browsertrix) without clobbering a
    /// curator's edits or an earlier seed — "fill gaps, curator wins." Returns
    /// whether it actually changed anything (so the caller only rewrites the
    /// finding aid on a real change, preserving hand formatting on a no-op).
    fn apply_to_empty(&self, c: &mut Collection) -> bool {
        let mut changed = false;
        let mut fill = |slot: &mut Option<String>, val: &Option<String>| {
            if slot.is_none() {
                if let Some(v) = val {
                    *slot = Some(v.clone());
                    changed = true;
                }
            }
        };
        fill(&mut c.description, &self.description);
        fill(&mut c.curator, &self.curator);
        fill(&mut c.creator, &self.creator);
        fill(&mut c.dates, &self.dates);
        fill(&mut c.rights, &self.rights);
        if c.subjects.is_empty() {
            if let Some(v) = &self.subjects {
                if !v.is_empty() {
                    c.subjects = v.clone();
                    changed = true;
                }
            }
        }
        if c.narrative
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            if let Some(v) = &self.narrative {
                c.narrative = Some(v.clone());
                changed = true;
            }
        }
        changed
    }
}

/// The YAML front-matter of a `collections/<slug>/README.md` finding aid. The `id` is the
/// filename stem (not stored here); the `narrative` is the Markdown body.
#[derive(Debug, Serialize, Deserialize, Default)]
struct FrontMatter {
    // `name` and `created` are `default` so a hand-authored file can omit them:
    // `name` falls back to the filename stem (see `parse_finding_aid`), and a
    // missing `created` is tolerated rather than failing the whole manifest load.
    #[serde(default)]
    name: String,
    #[serde(default)]
    created: String,
    // Custody. Unlike the curatorial fields below these are NOT written as
    // blanks: they're recorded by indice, not filled in by a curator, so an
    // empty scaffold would just be noise in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_by: Option<crate::identity::SubjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_by: Option<crate::identity::SubjectId>,
    // The DACS/EAD curatorial fields are written as `String`/`Vec` (not skipped
    // when empty) so an unset field appears as a blank the curator can fill in —
    // `creator: ''`, `subjects: []`. On read, blanks map back to `None`/empty in
    // `parse_finding_aid`, so ingest still seeds them and the "still needed"
    // nudge still fires; the blanks are a display scaffold, not real values.
    #[serde(default)]
    description: String,
    #[serde(default)]
    creator: String,
    #[serde(default)]
    dates: String,
    #[serde(default)]
    rights: String,
    #[serde(default)]
    subjects: Vec<String>,
    // `curator` is the instance/repository operator (often set once, not a
    // per-collection gap), so it stays optional and is omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    curator: Option<String>,
}

/// A validated collection id (slug).
///
/// Collection ids become directory names under `<home>/collections/`, so an
/// unchecked one is a path-traversal waiting to happen — and the check used to
/// be repeated by hand at every HTTP handler that touched a path. This type
/// makes that impossible to forget: the inner `String` is private, so the only
/// ways to obtain a `CollectionId` are [`parse`](Self::parse) (validating) and
/// [`from_name`](Self::from_name) (slugifying). Every path builder below takes
/// `&CollectionId` rather than `&str`, so code that tries to build a collection
/// path from raw request input simply does not compile.
///
/// Valid ids are non-empty ASCII alphanumerics plus `-` — no `/`, no `.`, so
/// neither an absolute path nor a `..` component can appear.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct CollectionId(String);

/// The "not set yet" placeholder, used for a legacy manifest record that predates
/// collection membership (see [`Manifest::open`], which heals these on load).
///
/// It has to satisfy two constraints at once. It must be a *valid* id — a
/// `Default` that produced something [`CollectionId::parse`] rejects would be a
/// hole in the type's promise. And it must be one [`slugify`] can never emit, so
/// it can't collide with a real collection: slugify only ever produces
/// `[a-z0-9]` separated by single dashes and never a *leading* dash, so a
/// leading dash makes this unambiguous (and is still a safe path component).
impl Default for CollectionId {
    fn default() -> Self {
        CollectionId("-unset".to_string())
    }
}

impl CollectionId {
    /// Validate an existing id (e.g. from a URL path segment or the manifest).
    /// `None` if it isn't a safe single path component.
    pub fn parse(s: &str) -> Option<Self> {
        let ok = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        ok.then(|| CollectionId(s.to_string()))
    }

    /// Derive an id from a human collection name (`"Bay Area Transit"` ->
    /// `bay-area-transit`). Infallible: [`slugify`] only emits safe characters.
    pub fn from_name(name: &str) -> Self {
        CollectionId(slugify(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// Read-only ergonomics: a CollectionId is usable anywhere a &str is expected
// (format!, comparisons, url_encode…). The conversion is deliberately one-way —
// there is no `From<&str>`, so the validating constructors stay the only way in.
impl std::ops::Deref for CollectionId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for CollectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl AsRef<str> for CollectionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl PartialEq<str> for CollectionId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}
impl PartialEq<&str> for CollectionId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}
impl PartialEq<String> for CollectionId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

/// Ids on disk were written by [`slugify`], so they should always be valid;
/// rejecting anything else means a hand-edited manifest can't smuggle a path
/// component past the type.
/// Field deserializer for [`Wacz::collection`]: tolerate what older manifests
/// actually contain. An absent key, an empty string, or (defensively) a
/// malformed id becomes the "unset" sentinel, which [`Manifest::open`] then
/// heals — rather than failing the whole load and taking the server down with
/// it. The strict [`CollectionId`] impl still guards every other id.
fn lenient_collection<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<CollectionId, D::Error> {
    let raw = String::deserialize(d)?;
    if raw.is_empty() {
        return Ok(CollectionId::default());
    }
    Ok(CollectionId::parse(&raw).unwrap_or_else(|| {
        tracing::warn!(id = %raw, "manifest holds an invalid collection id; treating it as unset");
        CollectionId::default()
    }))
}

impl<'de> Deserialize<'de> for CollectionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        CollectionId::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "invalid collection id {s:?}: expected ASCII letters, digits and '-' only"
            ))
        })
    }
}

/// The committable directory for a collection: `<home>/collections/<slug>/`,
/// holding its `README.md` finding aid plus any committable assets (per-crawl
/// notes, pinned/collection thumbnails).
pub fn collection_dir(home: &Path, slug: &CollectionId) -> PathBuf {
    home.join("collections").join(slug.as_str())
}

/// Migrate any flat `collections/<slug>.md` finding aids into the per-collection
/// directory form `collections/<slug>/README.md`. Best-effort and idempotent: a
/// flat file is moved only when its target `README.md` doesn't already exist.
fn migrate_flat_finding_aids(collections_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(collections_dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_some_and(|x| x == "md") && path.is_file() {
            let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let target = collections_dir.join(slug).join("README.md");
            if !target.exists() {
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::rename(&path, &target);
            }
        }
    }
}

/// Whether `collections_dir` holds at least one `<slug>/README.md` finding aid.
fn dir_has_findingaid(collections_dir: &Path) -> bool {
    std::fs::read_dir(collections_dir)
        .map(|rd| rd.flatten().any(|e| e.path().join("README.md").is_file()))
        .unwrap_or(false)
}

/// Whether a legacy `collections.json` array's first element looks like a WACZ
/// record (has `source`/`path`) rather than a collection group.
fn legacy_json_holds_waczs(path: &Path) -> Result<bool> {
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(value
        .as_array()
        .and_then(|a| a.first())
        .map(|e| e.get("source").is_some() || e.get("path").is_some())
        .unwrap_or(false))
}

/// Load every `collections/<slug>/README.md` finding aid (the descriptive source
/// of truth), sorted by id for a stable order.
pub fn load_finding_aids(collections_dir: &Path) -> Result<Vec<Collection>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(collections_dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("README.md").is_file())
        .collect();
    dirs.sort();
    let mut out = Vec::with_capacity(dirs.len());
    for dir in dirs {
        // The directory name IS the collection id, so it has to satisfy the
        // same rule as any other id; skip (loudly) anything that doesn't rather
        // than trusting a hand-made directory.
        let Some(id) = dir
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(CollectionId::parse)
        else {
            tracing::warn!(dir = %dir.display(), "skipping collection directory with an invalid id");
            continue;
        };
        let readme = dir.join("README.md");
        let text = std::fs::read_to_string(&readme)
            .with_context(|| format!("reading finding aid {}", readme.display()))?;
        out.push(
            parse_finding_aid(&id, &text)
                .with_context(|| format!("parsing finding aid {}", readme.display()))?,
        );
    }
    Ok(out)
}

/// Parse a finding aid's text (YAML front-matter + Markdown body) into a
/// [`Collection`] with the given `id` (the collection directory name).
fn parse_finding_aid(id: &CollectionId, text: &str) -> Result<Collection> {
    let (fm_src, body) = split_front_matter(text);
    let fm: FrontMatter = if fm_src.trim().is_empty() {
        FrontMatter::default()
    } else {
        serde_yaml_ng::from_str(fm_src).context("parsing YAML front-matter")?
    };
    let narrative = {
        let b = body.trim();
        (!b.is_empty()).then(|| b.to_string())
    };
    // A blank scaffold field (`creator: ''`) reads back as unset, so ingest
    // still seeds it and the "still needed" nudge still fires.
    let blank_none = |s: String| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    Ok(Collection {
        id: id.clone(),
        name: if fm.name.is_empty() {
            id.to_string()
        } else {
            fm.name
        },
        description: blank_none(fm.description),
        created: fm.created,
        created_by: fm.created_by,
        updated_by: fm.updated_by,
        curator: fm.curator,
        creator: blank_none(fm.creator),
        dates: blank_none(fm.dates),
        rights: blank_none(fm.rights),
        subjects: fm.subjects,
        narrative,
    })
}

/// Split leading `---`-delimited YAML front-matter from the Markdown body,
/// returning `(front_matter_yaml, body)`. Front matter is `""` when absent (or
/// when the opening fence has no matching close).
fn split_front_matter(text: &str) -> (&str, &str) {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text); // tolerate a BOM
    let after_open = match t
        .strip_prefix("---\n")
        .or_else(|| t.strip_prefix("---\r\n"))
    {
        Some(r) => r,
        None => return ("", t),
    };
    let mut idx = 0;
    for line in after_open.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return (&after_open[..idx], &after_open[idx + line.len()..]);
        }
        idx += line.len();
    }
    ("", t) // unterminated front matter → treat the whole file as body
}

/// Write a collection's finding aid to `<home>/collections/<slug>/README.md`:
/// YAML front-matter for the structured fields, then the Markdown `narrative`
/// body.
pub fn write_finding_aid(home: &Path, c: &Collection) -> Result<()> {
    let dir = collection_dir(home, &c.id);
    std::fs::create_dir_all(&dir)?;
    let fm = FrontMatter {
        name: c.name.clone(),
        created: c.created.clone(),
        created_by: c.created_by.clone(),
        updated_by: c.updated_by.clone(),
        // Unset curatorial fields become empty blanks in the file (scaffold).
        description: c.description.clone().unwrap_or_default(),
        creator: c.creator.clone().unwrap_or_default(),
        dates: c.dates.clone().unwrap_or_default(),
        rights: c.rights.clone().unwrap_or_default(),
        subjects: c.subjects.clone(),
        curator: c.curator.clone(),
    };
    let yaml = serde_yaml_ng::to_string(&fm).context("serializing YAML front-matter")?;
    let mut out = String::from("---\n");
    out.push_str(&yaml);
    if !yaml.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n");
    if let Some(body) = c
        .narrative
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        out.push('\n');
        out.push_str(body);
        out.push('\n');
    }
    let path = dir.join("README.md");
    // Atomically: this is hand-written curatorial work, and the directory is
    // meant to be committed, so a half-written README is both a data loss and
    // a confusing diff.
    crate::fsio::write_atomic_str(&path, &out)
        .with_context(|| format!("writing finding aid {}", path.display()))?;
    Ok(())
}

/// Path to a crawl's committable Markdown note, under its collection:
/// `<home>/collections/<slug>/crawls/<id>.md`.
pub fn crawl_note_path(home: &Path, collection: &CollectionId, id: &str) -> PathBuf {
    collection_dir(home, collection)
        .join("crawls")
        .join(format!("{id}.md"))
}

/// Path to a crawl's curator-pinned thumbnail (committable, downscaled JPEG):
/// `<home>/collections/<slug>/crawls/<id>.jpg`. Its *presence* is the pin marker
/// — a pinned image lives with the finding aid and is never overwritten by
/// (re)indexing (which only writes the auto cache under `index/thumbs/`).
pub fn pinned_thumb_path(home: &Path, collection: &CollectionId, id: &str) -> PathBuf {
    collection_dir(home, collection)
        .join("crawls")
        .join(format!("{id}.jpg"))
}

/// Path to a collection's curator-set representative thumbnail (committable):
/// `<home>/collections/<slug>/thumbnail.jpg`.
pub fn collection_thumb_path(home: &Path, collection: &CollectionId) -> PathBuf {
    collection_dir(home, collection).join("thumbnail.jpg")
}

/// Read a crawl's Markdown note, if present and non-empty.
pub fn read_crawl_note(home: &Path, collection: &CollectionId, id: &str) -> Option<String> {
    let text = std::fs::read_to_string(crawl_note_path(home, collection, id)).ok()?;
    let t = text.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Write a crawl's Markdown note to `<home>/collections/<slug>/crawls/<id>.md`.
pub fn write_crawl_note(
    home: &Path,
    collection: &CollectionId,
    id: &str,
    note: &str,
) -> Result<()> {
    let path = crawl_note_path(home, collection, id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::fsio::write_atomic_str(&path, &format!("{}\n", note.trim()))
        .with_context(|| format!("writing crawl note {}", path.display()))?;
    Ok(())
}

/// Deserialize `software` as either a single string (older manifests wrote one)
/// or a list of strings, always yielding a `Vec<String>`.
fn string_or_seq<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// A URL/id-friendly slug for a collection name: lowercase ASCII alphanumerics,
/// with runs of anything else collapsed to a single hyphen and trimmed
/// (`"Bay Area Transit"` -> `bay-area-transit`). Falls back to a short hash when
/// the name has no sluggable characters.
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash {
                slug.push('-');
                pending_dash = false;
            }
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        bytes_to_hex(&sha256_of_bytes(name.as_bytes())[..4])
    } else {
        slug
    }
}

/// Stable short ID for a WACZ: first 8 hex chars of SHA-256 of the source
/// location string (an absolute file path or a URL).
pub fn wacz_id(source: &Source) -> String {
    let hash = sha256_of_bytes(source.location().as_bytes());
    bytes_to_hex(&hash[..4])
}

/// Compute SHA-256 of a file's contents, reading in 64 KiB chunks.
pub fn file_sha256(path: &Path) -> Result<String> {
    use sha2::Digest;
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(bytes_to_hex(hasher.finalize().as_slice()))
}

fn sha256_of_bytes(data: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn collection_id_rejects_unsafe_and_default_is_valid() {
        for bad in ["", ".", "..", "a/b", "../evil", "a.b", "a b", "café"] {
            assert!(CollectionId::parse(bad).is_none(), "should reject {bad:?}");
        }
        for good in ["news", "bay-area-transit", "a1b2c3d4"] {
            assert_eq!(CollectionId::parse(good).unwrap().as_str(), good);
        }
        // from_name always yields something parse accepts...
        assert!(
            CollectionId::parse(CollectionId::from_name("Bay Area / Transit!").as_str()).is_some()
        );
        // ...and so must Default, or the type's promise has a hole in it.
        assert!(CollectionId::parse(CollectionId::default().as_str()).is_some());
    }

    /// The property that actually matters, stated in terms of the filesystem
    /// rather than the allowlist: joining a `CollectionId` onto a base directory
    /// must land *inside* it, as exactly one new component. Restating "only
    /// alnum and dash" in the test would just assert the implementation back at
    /// itself; this asserts the guarantee callers depend on.
    fn lands_inside_base(id: &CollectionId) -> bool {
        let base = Path::new("/base/collections");
        let joined = base.join(id.as_str());
        joined.starts_with(base)
            && joined.parent() == Some(base)
            && joined.components().count() == base.components().count() + 1
    }

    /// Exhaustive over the characters that can actually cause trouble, rather
    /// than random fuzzing: the danger space here is tiny, so every string up to
    /// length 3 over a nasty alphabet is both cheap and far more thorough than
    /// sampling. Anything `parse` accepts must satisfy the path property.
    #[test]
    fn parse_accepts_only_safe_single_path_components() {
        let alphabet = [
            'a', '1', '-', '.', '/', '\\', ' ', ':', '\0', '~', '*',
            '\u{ff0f}', // fullwidth solidus
        ];
        let n = alphabet.len();
        let mut checked = 0usize;
        let mut accepted = 0usize;
        let mut buf = String::new();
        for len in 0..=3u32 {
            // Base-n counting over the alphabet: provably enumerates every
            // string of this length exactly once.
            for mut code in 0..n.pow(len) {
                buf.clear();
                for _ in 0..len {
                    buf.push(alphabet[code % n]);
                    code /= n;
                }
                checked += 1;
                if let Some(id) = CollectionId::parse(&buf) {
                    accepted += 1;
                    assert!(
                        lands_inside_base(&id),
                        "parse accepted {buf:?} but it escapes its base directory"
                    );
                }
            }
        }
        assert_eq!(
            checked,
            1 + n + n * n + n * n * n,
            "the sweep must be exhaustive"
        );
        assert!(accepted > 0, "the sweep must include accepted inputs too");
    }

    /// Whatever `from_name` produces — from any input, including hostile ones —
    /// must also satisfy the path property, since it bypasses `parse`.
    #[test]
    fn from_name_output_is_always_a_safe_component() {
        for name in [
            "../../etc/passwd",
            "/absolute/path",
            "..",
            ".",
            "",
            "   ",
            "C:\\Windows\\system32",
            "a/../../b",
            "\u{ff0f}\u{ff0f}",
            "🙂🙂🙂",
            "..%2f..%2fetc",
            &"x".repeat(500),
        ] {
            let id = CollectionId::from_name(name);
            assert!(
                lands_inside_base(&id),
                "from_name({name:?}) produced {id:?}, which escapes its base"
            );
            assert!(
                CollectionId::parse(id.as_str()).is_some(),
                "from_name({name:?}) produced {id:?}, which parse rejects"
            );
        }
    }

    /// A legacy `collections.json` (the oldest layout, WACZ records inline and
    /// no `collection` key) must still land its crawls in the synthesized
    /// singleton collection — regressed once when `CollectionId::default()`
    /// stopped being the empty string and the migration guard stopped matching.
    #[test]
    fn legacy_wacz_records_are_adopted_by_their_synthesized_collection() {
        let tmp = TempDir::new().unwrap();
        let idx = tmp.path().join("index");
        std::fs::create_dir_all(&idx).unwrap();
        std::fs::write(
            idx.join("collections.json"),
            r#"[{"id":"abc12345","name":"Old","path":"archive/a.wacz","date_indexed":"2026-01-01T00:00:00Z","file_size":1,"sha256":"x"}]"#,
        )
        .unwrap();
        let m = Manifest::open(&idx).unwrap();
        let id = cid("abc12345");
        assert_eq!(
            m.waczs[0].collection, id,
            "the WACZ id doubles as its collection"
        );
        assert_eq!(
            m.members_of(&id).count(),
            1,
            "the crawl must not be orphaned"
        );
    }

    /// A manifest holding an empty (or malformed) collection id must load and be
    /// healed, not abort `Manifest::open` and take every page down with it.
    #[test]
    fn empty_or_bad_collection_id_is_healed_not_fatal() {
        for raw in [r#""""#, r#""My_Coll""#] {
            let tmp = TempDir::new().unwrap();
            let idx = tmp.path().join("index");
            std::fs::create_dir_all(&idx).unwrap();
            std::fs::write(
                idx.join("waczs.json"),
                format!(r#"[{{"id":"abc12345","collection":{raw},"name":"X","source":"archive/a.wacz","date_indexed":"2026-01-01T00:00:00Z","file_size":1,"sha256":"x"}}]"#),
            )
            .unwrap();
            let m = Manifest::open(&idx).expect("a bad id must not brick the manifest");
            assert_eq!(
                m.waczs[0].collection,
                cid("abc12345"),
                "healed from the WACZ id"
            );
        }
    }

    /// One unreadable record in a legacy `collections.json` must not discard
    /// every curator's description/narrative.
    #[test]
    fn one_bad_collection_record_does_not_discard_the_rest() {
        let tmp = TempDir::new().unwrap();
        let idx = tmp.path().join("index");
        std::fs::create_dir_all(&idx).unwrap();
        std::fs::write(
            idx.join("waczs.json"),
            r#"[{"id":"abc12345","collection":"good","name":"X","source":"archive/a.wacz","date_indexed":"2026-01-01T00:00:00Z","file_size":1,"sha256":"x"}]"#,
        )
        .unwrap();
        std::fs::write(
            idx.join("collections.json"),
            r#"[{"id":"good","name":"Good","created":"2026-01-01T00:00:00Z","description":"kept"},
                {"id":"My_Coll","name":"Bad","created":"2026-01-01T00:00:00Z"}]"#,
        )
        .unwrap();
        let m = Manifest::open(&idx).unwrap();
        let ids: Vec<String> = m.collections.iter().map(|c| c.id.to_string()).collect();
        assert!(
            ids.contains(&"good".to_string()),
            "the valid record survives: {ids:?}"
        );
        assert_eq!(
            m.collections
                .iter()
                .find(|c| c.id == "good")
                .unwrap()
                .description
                .as_deref(),
            Some("kept"),
            "its curator metadata survives too"
        );
    }

    /// The sentinel must be a valid id (the type's promise) *and* unreachable
    /// from slugify, so it can never collide with a real collection.
    #[test]
    fn unset_sentinel_is_valid_but_unslugifiable() {
        let d = CollectionId::default();
        assert!(
            CollectionId::parse(d.as_str()).is_some(),
            "sentinel must satisfy parse"
        );
        for name in ["unset", "Unset", "UNSET", "-unset", " unset ", "un set"] {
            assert_ne!(
                CollectionId::from_name(name),
                d,
                "slugify({name:?}) must not equal the sentinel"
            );
        }
    }

    /// A valid collection id for tests.
    fn cid(s: &str) -> CollectionId {
        CollectionId::parse(s).expect("valid test id")
    }

    use super::*;
    use tempfile::TempDir;

    #[test]
    fn wacz_id_is_stable() {
        let s = Source::File(PathBuf::from("/data/archive.wacz"));
        let id1 = wacz_id(&s);
        let id2 = wacz_id(&s);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 8);
    }

    #[test]
    fn different_sources_different_ids() {
        let id1 = wacz_id(&Source::File(PathBuf::from("/data/a.wacz")));
        let id2 = wacz_id(&Source::File(PathBuf::from("/data/b.wacz")));
        let id3 = wacz_id(&Source::Url("https://ex.org/a.wacz".to_string()));
        assert_ne!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn source_parse_distinguishes_url_from_path() {
        assert!(Source::parse("https://ex.org/a.wacz").is_url());
        assert!(Source::parse("http://ex.org/a.wacz").is_url());
        assert!(!Source::parse("/data/a.wacz").is_url());
        assert!(!Source::parse("relative/a.wacz").is_url());
    }

    #[test]
    fn source_serializes_as_plain_string() {
        let file = Source::File(PathBuf::from("/data/a.wacz"));
        assert_eq!(serde_json::to_string(&file).unwrap(), "\"/data/a.wacz\"");
        let url = Source::Url("https://ex.org/a.wacz".to_string());
        assert_eq!(
            serde_json::to_string(&url).unwrap(),
            "\"https://ex.org/a.wacz\""
        );
        // Round-trips back to the right variant.
        let back: Source = serde_json::from_str("\"https://ex.org/a.wacz\"").unwrap();
        assert_eq!(back, url);
    }

    #[test]
    fn browsertrix_source_roundtrips() {
        let s = Source::Browsertrix {
            host: "https://app.browsertrix.com".into(),
            org: "o1".into(),
            item: "item-1".into(),
            resource: "crawl-20250101-abc-0.wacz".into(),
        };
        let encoded = s.location();
        assert_eq!(
            encoded,
            "browsertrix|https://app.browsertrix.com|o1|item-1|crawl-20250101-abc-0.wacz"
        );
        assert_eq!(Source::parse(&encoded), s);
        assert!(s.is_remote());
        assert!(s.as_file().is_none());
        assert!(!s.is_url());
        // A stable id (independent of the presigned URL, which changes each time).
        assert_eq!(wacz_id(&s), wacz_id(&Source::parse(&encoded)));
    }

    #[test]
    fn browsertrix_public_source_roundtrips() {
        let s = Source::BrowsertrixPublic {
            host: "https://app.browsertrix.com".into(),
            org: "o9".into(),
            collection: "col-uuid".into(),
            resource: "crawl-20250101-abc-0.wacz".into(),
        };
        let encoded = s.location();
        assert_eq!(
            encoded,
            "browsertrix-public|https://app.browsertrix.com|o9|col-uuid|crawl-20250101-abc-0.wacz"
        );
        assert_eq!(Source::parse(&encoded), s);
        assert!(s.is_remote());
        assert!(s.as_file().is_none());
        assert!(!s.is_url());
        assert!(s.resolve(std::path::Path::new("/home")).is_none());
        // Distinct from the private variant with the same host/resource, and a
        // stable id across a parse round-trip.
        assert_ne!(
            wacz_id(&s),
            wacz_id(&Source::Browsertrix {
                host: "https://app.browsertrix.com".into(),
                org: "o9".into(),
                item: "col-uuid".into(),
                resource: "crawl-20250101-abc-0.wacz".into(),
            })
        );
        assert_eq!(wacz_id(&s), wacz_id(&Source::parse(&encoded)));
    }

    #[test]
    fn software_accepts_string_or_list() {
        // Older manifests wrote `software` as a single string; newer ones a list.
        let legacy: Wacz = serde_json::from_str(
            r#"{"id":"a","source":"archive/x.wacz","name":"x","date_indexed":"t","file_size":1,"sha256":"h","software":"py-wacz 0.4.6"}"#,
        ).unwrap();
        assert_eq!(legacy.software, vec!["py-wacz 0.4.6".to_string()]);

        let listy: Wacz = serde_json::from_str(
            r#"{"id":"a","source":"archive/x.wacz","name":"x","date_indexed":"t","file_size":1,"sha256":"h","software":["Heritrix/3.4.0","py-wacz 0.4.6"]}"#,
        ).unwrap();
        assert_eq!(
            listy.software,
            vec!["Heritrix/3.4.0".to_string(), "py-wacz 0.4.6".to_string()]
        );

        // Absent -> empty, and empty is not serialized back out.
        let none: Wacz = serde_json::from_str(
            r#"{"id":"a","source":"archive/x.wacz","name":"x","date_indexed":"t","file_size":1,"sha256":"h"}"#,
        ).unwrap();
        assert!(none.software.is_empty());
        assert!(!serde_json::to_string(&none).unwrap().contains("software"));
    }

    #[test]
    fn manifest_reads_legacy_path_key() {
        // Older manifests used "path" instead of "source".
        let tmp = TempDir::new().unwrap();
        let legacy = r#"[{"id":"abc12345","path":"/data/old.wacz","name":"old","date_indexed":"2026-07-01T00:00:00Z","file_size":10,"sha256":"deadbeef"}]"#;
        std::fs::write(tmp.path().join("collections.json"), legacy).unwrap();
        let m = Manifest::open(tmp.path()).unwrap();
        assert_eq!(m.waczs.len(), 1);
        assert_eq!(
            m.waczs[0].source,
            Source::File(PathBuf::from("/data/old.wacz"))
        );
        // Migration synthesizes a singleton collection per legacy WACZ.
        assert_eq!(m.collections.len(), 1);
    }

    #[test]
    fn file_sha256_detects_content_change() {
        // The fixity primitive behind `indice verify`: the same bytes hash to
        // the same digest, and a single changed byte changes the digest.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data.bin");
        std::fs::write(&path, b"hello world").unwrap();

        let h1 = file_sha256(&path).unwrap();
        let h2 = file_sha256(&path).unwrap();
        assert_eq!(h1, h2, "unchanged file should hash identically");
        assert_eq!(h1.len(), 64, "sha-256 hex is 64 chars");

        std::fs::write(&path, b"hello worlx").unwrap();
        let h3 = file_sha256(&path).unwrap();
        assert_ne!(h1, h3, "a changed byte must change the digest");
    }

    #[test]
    fn collection_custody_round_trips_through_the_finding_aid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let alice = crate::identity::SubjectId::parse("alice@x.edu").unwrap();
        let c = Collection {
            id: cid("notes"),
            name: "Notes".into(),
            created: "2026-01-01T00:00:00Z".into(),
            created_by: Some(alice.clone()),
            updated_by: Some(alice.clone()),
            ..Default::default()
        };
        write_finding_aid(tmp.path(), &c).unwrap();
        let text =
            std::fs::read_to_string(collection_dir(tmp.path(), &c.id).join("README.md")).unwrap();
        // Stored as the canonical identity, for comparison — the page resolves
        // a display name through users.yaml at render time.
        assert!(text.contains("created_by: mailto:alice@x.edu"), "{text}");
        let back = parse_finding_aid(&c.id, &text).unwrap();
        assert_eq!(back.created_by, Some(alice));

        // A finding aid with no custody keys must load, and must not grow them
        // on the way back out — otherwise every hand-written one churns.
        let plain = "---\nname: Notes\ncreated: 2026-01-01T00:00:00Z\n---\n";
        let none = parse_finding_aid(&cid("notes"), plain).unwrap();
        assert_eq!(none.created_by, None);
        write_finding_aid(tmp.path(), &none).unwrap();
        let out = std::fs::read_to_string(collection_dir(tmp.path(), &none.id).join("README.md"))
            .unwrap();
        assert!(!out.contains("created_by"), "{out}");
    }

    #[test]
    fn update_persists_on_success_and_leaves_the_file_alone_on_error() {
        let tmp = TempDir::new().unwrap();
        let idx = tmp.path().join("index");
        std::fs::create_dir_all(&idx).unwrap();

        // A successful update is saved without the caller remembering to.
        let id = Manifest::update(&idx, |m| {
            m.upsert_wacz(wacz("abc12345", "First", None));
            Ok(m.waczs[0].id.clone())
        })
        .unwrap();
        assert_eq!(id, "abc12345");
        assert_eq!(Manifest::open(&idx).unwrap().waczs.len(), 1);

        // A failing one leaves the previous state exactly as it was, rather
        // than persisting a half-applied change.
        let err = Manifest::update(&idx, |m| -> Result<()> {
            m.upsert_wacz(wacz("def67890", "Second", None));
            anyhow::bail!("changed my mind")
        });
        assert!(err.is_err());
        let after = Manifest::open(&idx).unwrap();
        assert_eq!(after.waczs.len(), 1, "the aborted change must not persist");
        assert_eq!(after.waczs[0].id, "abc12345");
    }

    #[test]
    fn a_manifest_without_custody_still_loads() {
        // added_by is additive, so every manifest written before it existed
        // must deserialize unchanged — and round-trip without gaining a key,
        // or every entry would churn in the next git diff.
        // Uses the older `path` key too, so this is a genuinely old entry
        // rather than a today's-shape one with a field removed.
        let line = r#"{"id":"abc","collection":"c","path":"/a/b.wacz","name":"n","date_indexed":"2026-01-01T00:00:00Z","file_size":1,"sha256":"x"}"#;
        let w: Wacz = serde_json::from_str(line).expect("legacy entry parses");
        assert_eq!(w.added_by, None, "unattributed, not invented");
        let back = serde_json::to_string(&w).unwrap();
        assert!(
            !back.contains("added_by"),
            "an unattributed entry must not grow the key: {back}"
        );
    }

    /// A WACZ member with the given id/name and defaults elsewhere.
    fn wacz(id: &str, name: &str, description: Option<&str>) -> Wacz {
        Wacz {
            id: id.to_string(),
            collection: cid(id),
            source: Source::File(PathBuf::from("/data/test.wacz")),
            name: name.to_string(),
            date_indexed: "2026-07-01T00:00:00Z".to_string(),
            file_size: 1024,
            sha256: "deadbeef".to_string(),
            description: description.map(str::to_string),
            crawl_date: None,
            seed_pages: vec![],
            software: Vec::new(),
            operator: None,
            user_agent: None,
            robots: None,
            page_count: None,
            capture_start: None,
            capture_end: None,
            browsertrix: None,
            archive_it: None,
            nested_waczs: None,
            modified: None,
            is_part_of: None,
            hostname: None,
            conforms_to: None,
            keywords: Vec::new(),
            licenses: Vec::new(),
            status_counts: BTreeMap::new(),
            added_by: None,
        }
    }

    #[test]
    fn wacz_without_browsertrix_field_deserializes_to_none() {
        // Backward compatibility: an older collections.json entry has no
        // `browsertrix` key; it must load with the field defaulting to None.
        let json = r#"{
            "id": "abc12345",
            "source": "/data/test.wacz",
            "name": "test",
            "date_indexed": "2026-07-01T00:00:00Z",
            "file_size": 1024,
            "sha256": "deadbeef"
        }"#;
        let w: Wacz = serde_json::from_str(json).unwrap();
        assert!(w.browsertrix.is_none());
    }

    #[test]
    fn browsertrix_ref_roundtrips() {
        let w = {
            let mut w = wacz("abc12345", "test", None);
            w.browsertrix = Some(BrowsertrixRef {
                host: "https://app.browsertrix.com".to_string(),
                item_id: "item-1".to_string(),
                resource_hash: "sha256:aa".to_string(),
                review_status: Some(4),
            });
            w
        };
        let json = serde_json::to_string(&w).unwrap();
        let back: Wacz = serde_json::from_str(&json).unwrap();
        assert_eq!(w.browsertrix, back.browsertrix);
    }

    #[test]
    fn manifest_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut m = Manifest::open(tmp.path()).unwrap();
        assert!(m.waczs.is_empty());

        m.upsert_wacz(wacz("abc12345", "test", Some("A test collection")));
        m.save().unwrap();

        let m2 = Manifest::open(tmp.path()).unwrap();
        assert_eq!(m2.waczs.len(), 1);
        assert_eq!(m2.waczs[0].id, "abc12345");
        assert_eq!(
            m2.waczs[0].description.as_deref(),
            Some("A test collection")
        );
    }

    #[test]
    fn manifest_upsert_updates_existing() {
        let tmp = TempDir::new().unwrap();
        let mut m = Manifest::open(tmp.path()).unwrap();

        m.upsert_wacz(wacz("abc12345", "test", None));
        let mut updated = wacz("abc12345", "test-updated", Some("updated"));
        updated.sha256 = "cafebabe".to_string();
        m.upsert_wacz(updated);

        assert_eq!(m.waczs.len(), 1);
        assert_eq!(m.waczs[0].name, "test-updated");
    }

    #[test]
    fn slugify_makes_readable_ids() {
        assert_eq!(slugify("Bay Area Transit"), "bay-area-transit");
        assert_eq!(slugify("  Hello, World!  "), "hello-world");
        assert_eq!(slugify("already-slug"), "already-slug");
        // No sluggable characters -> short hash fallback (8 hex chars).
        assert_eq!(slugify("!!!").len(), 8);
    }

    #[test]
    fn seed_fields_fills_only_empty_and_never_clobbers() {
        let tmp = TempDir::new().unwrap();
        let mut m = Manifest::open(&tmp.path().join("index")).unwrap();

        // First seed populates empty fields.
        m.seed_fields(
            &cid("sucho"),
            "SUCHO",
            &CollectionFields {
                narrative: Some("first scope".into()),
                subjects: Some(vec!["ukraine".into()]),
                ..Default::default()
            },
            "2026-01-01T00:00:00Z",
        );
        let c = m.collection_by_id("sucho").unwrap();
        assert_eq!(c.narrative.as_deref(), Some("first scope"));
        assert_eq!(c.subjects, vec!["ukraine"]);

        // A curator refines the narrative.
        m.apply_fields(
            "SUCHO",
            &CollectionFields {
                narrative: Some("curator's words".into()),
                ..Default::default()
            },
            "2026-01-01T00:00:00Z",
            None,
        );

        // A later seed fills a still-empty field (creator) but must NOT overwrite
        // the set ones (narrative, subjects).
        m.seed_fields(
            &cid("sucho"),
            "SUCHO",
            &CollectionFields {
                narrative: Some("second scope".into()),
                subjects: Some(vec!["other".into()]),
                creator: Some("SUCHO team".into()),
                ..Default::default()
            },
            "2026-02-02T00:00:00Z",
        );
        let c = m.collection_by_id("sucho").unwrap();
        assert_eq!(
            c.narrative.as_deref(),
            Some("curator's words"),
            "edit preserved"
        );
        assert_eq!(c.subjects, vec!["ukraine"], "subjects preserved");
        assert_eq!(
            c.creator.as_deref(),
            Some("SUCHO team"),
            "empty field seeded"
        );
    }

    #[test]
    fn finding_aid_scaffolds_empty_curatorial_fields() {
        let tmp = TempDir::new().unwrap();
        // A collection with nothing filled in but its name/created.
        let c = Collection {
            id: cid("news"),
            name: "News".into(),
            created: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        write_finding_aid(tmp.path(), &c).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("collections/news/README.md")).unwrap();

        // The DACS front-matter fields appear as blanks for the curator to fill.
        for blank in ["creator: ''", "dates: ''", "rights: ''", "description: ''"] {
            assert!(
                text.contains(blank),
                "expected scaffold {blank:?} in:\n{text}"
            );
        }
        assert!(text.contains("subjects: []"), "subjects scaffold:\n{text}");

        // Reading it back, the blanks are unset — so ingest still seeds them and
        // the "still needed" nudge still fires (they're a display scaffold only).
        let back = parse_finding_aid(&cid("news"), &text).unwrap();
        assert_eq!(back.creator, None);
        assert_eq!(back.dates, None);
        assert_eq!(back.rights, None);
        assert_eq!(back.description, None);
        assert!(back.subjects.is_empty());

        // A blank field is still fillable by a fill-gaps seed.
        assert!(CollectionFields {
            creator: Some("Acme".into()),
            ..Default::default()
        }
        .apply_to_empty(&mut { back }));
    }

    #[test]
    fn apply_fields_creates_then_updates_preserving_created() {
        let tmp = TempDir::new().unwrap();
        let mut m = Manifest::open(&tmp.path().join("index")).unwrap();

        let id = m.apply_fields(
            "Bay Area Transit",
            &CollectionFields {
                description: Some("desc".into()),
                ..Default::default()
            },
            "2026-01-01T00:00:00Z",
            None,
        );
        assert_eq!(id, "bay-area-transit");
        assert_eq!(m.collections.len(), 1);
        assert_eq!(m.collections[0].description.as_deref(), Some("desc"));

        // Re-applying updates only the Some fields, keeps `created`, and — "fill
        // gaps, curator wins" — a None leaves the existing value untouched.
        m.apply_fields(
            "Bay Area Transit",
            &CollectionFields {
                description: None, // not provided → keep "desc"
                curator: Some("Ed".into()),
                creator: Some("BART".into()),
                subjects: Some(vec!["transit".into(), "bay-area".into()]),
                ..Default::default()
            },
            "2026-02-02T00:00:00Z",
            None,
        );
        assert_eq!(m.collections.len(), 1);
        assert_eq!(m.collections[0].description.as_deref(), Some("desc"));
        assert_eq!(m.collections[0].curator.as_deref(), Some("Ed"));
        assert_eq!(m.collections[0].creator.as_deref(), Some("BART"));
        assert_eq!(m.collections[0].subjects, vec!["transit", "bay-area"]);
        assert_eq!(m.collections[0].created, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn finding_aid_roundtrips_through_files() {
        let tmp = TempDir::new().unwrap();
        let index_dir = tmp.path().join("index");

        let mut m = Manifest::open(&index_dir).unwrap();
        m.apply_fields(
            "SUCHO",
            &CollectionFields {
                creator: Some("Saving Ukrainian Cultural Heritage Online".into()),
                dates: Some("2022–2023".into()),
                rights: Some("See individual sites; archived for research".into()),
                subjects: Some(vec!["ukraine".into(), "cultural heritage".into()]),
                narrative: Some("## Scope and Content\n\nWhy this was archived.".into()),
                ..Default::default()
            },
            "2026-01-01T00:00:00Z",
            None,
        );
        m.save().unwrap();

        // A finding-aid Markdown file is written under <home>/collections/<slug>/.
        let md = tmp.path().join("collections/sucho/README.md");
        assert!(md.exists(), "finding aid should be written to {md:?}");
        let text = std::fs::read_to_string(&md).unwrap();
        assert!(text.starts_with("---\n"), "has YAML front-matter");
        assert!(text.contains("creator: Saving Ukrainian"));
        assert!(text.contains("## Scope and Content"));

        // Re-opening reads the file back as the source of truth.
        let m2 = Manifest::open(&index_dir).unwrap();
        let c = m2.collection_by_id("sucho").unwrap();
        assert_eq!(
            c.creator.as_deref(),
            Some("Saving Ukrainian Cultural Heritage Online")
        );
        assert_eq!(c.subjects, vec!["ukraine", "cultural heritage"]);
        assert_eq!(
            c.narrative.as_deref(),
            Some("## Scope and Content\n\nWhy this was archived.")
        );
        assert_eq!(c.created, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn hand_edited_finding_aid_loads() {
        // A curator can author the Markdown by hand; indice reads it verbatim.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("collections/my-coll");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("README.md"),
            "---\nname: My Collection\ncreated: 2026-03-01T00:00:00Z\nsubjects:\n  - a\n  - b\n---\n\n# About\n\nHand-written prose.\n",
        )
        .unwrap();

        let m = Manifest::open(&tmp.path().join("index")).unwrap();
        let c = m.collection_by_id("my-coll").unwrap();
        assert_eq!(c.name, "My Collection");
        assert_eq!(c.subjects, vec!["a", "b"]);
        assert_eq!(
            c.narrative.as_deref(),
            Some("# About\n\nHand-written prose.")
        );
    }

    #[test]
    fn minimal_hand_edited_finding_aid_does_not_crash_load() {
        // A curator may omit `name`/`created`; that must not fail the whole
        // manifest load. `name` falls back to the filename stem.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("collections/sucho");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("README.md"),
            "---\ncreator: Someone\n---\n\n## Scope\n\nWhy.\n",
        )
        .unwrap();

        let m = Manifest::open(&tmp.path().join("index")).unwrap();
        let c = m
            .collection_by_id("sucho")
            .expect("loads despite missing name/created");
        assert_eq!(c.name, "sucho", "name falls back to the directory name");
        assert_eq!(c.creator.as_deref(), Some("Someone"));
    }

    #[test]
    fn legacy_collections_json_migrates_to_markdown() {
        // A pre-existing index/collections.json (groups, no collections/ dir)
        // loads and is migrated to a Markdown finding aid on save.
        let tmp = TempDir::new().unwrap();
        let index_dir = tmp.path().join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(
            index_dir.join("collections.json"),
            r#"[{"id":"old","name":"Old Coll","description":"legacy","created":"2025-01-01T00:00:00Z"}]"#,
        )
        .unwrap();
        // waczs.json present (so this isn't the waczs-in-collections layout).
        std::fs::write(index_dir.join("waczs.json"), "[]").unwrap();

        let m = Manifest::open(&index_dir).unwrap();
        assert_eq!(
            m.collection_by_id("old").unwrap().description.as_deref(),
            Some("legacy")
        );
        m.save().unwrap();
        assert!(tmp.path().join("collections/old/README.md").exists());
    }

    #[test]
    fn flat_finding_aid_migrates_to_subdir_on_open() {
        // A home from the earlier flat layout (collections/<slug>.md) is migrated
        // to collections/<slug>/README.md, preserving the curator's prose.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("collections");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sucho.md"),
            "---\nname: SUCHO\ncreated: 2026-01-01T00:00:00Z\n---\n\n## Scope\n\nWhy.\n",
        )
        .unwrap();

        let m = Manifest::open(&tmp.path().join("index")).unwrap();
        assert!(
            dir.join("sucho/README.md").is_file(),
            "flat file migrated into the collection dir"
        );
        assert!(!dir.join("sucho.md").exists(), "flat file removed");
        let c = m.collection_by_id("sucho").unwrap();
        assert_eq!(c.name, "SUCHO");
        assert_eq!(c.narrative.as_deref(), Some("## Scope\n\nWhy."));
    }

    #[test]
    fn migration_does_not_clobber_an_existing_readme() {
        // If a flat <slug>.md and an already-migrated <slug>/README.md coexist,
        // the migration must not overwrite the README (idempotent, safe).
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("collections");
        std::fs::create_dir_all(dir.join("sucho")).unwrap();
        std::fs::write(
            dir.join("sucho/README.md"),
            "---\nname: Kept\ncreated: 2026-01-01T00:00:00Z\n---\n\nThe real one.\n",
        )
        .unwrap();
        std::fs::write(dir.join("sucho.md"), "---\nname: Stale\n---\n\nOld flat.\n").unwrap();

        let m = Manifest::open(&tmp.path().join("index")).unwrap();
        // The subdir README wins; the flat file is left untouched (not moved over).
        assert_eq!(m.collection_by_id("sucho").unwrap().name, "Kept");
        assert!(
            dir.join("sucho.md").exists(),
            "flat file not clobbered onto README"
        );
    }

    #[test]
    fn crawl_note_roundtrips() {
        let tmp = TempDir::new().unwrap();
        assert!(read_crawl_note(tmp.path(), &cid("sucho"), "abc12345").is_none());
        write_crawl_note(
            tmp.path(),
            &cid("sucho"),
            "abc12345",
            "  A note about absences.  ",
        )
        .unwrap();
        // Stored under the collection dir, committable alongside the finding aid.
        assert!(tmp
            .path()
            .join("collections/sucho/crawls/abc12345.md")
            .is_file());
        assert_eq!(
            read_crawl_note(tmp.path(), &cid("sucho"), "abc12345").as_deref(),
            Some("A note about absences.")
        );
    }
}
