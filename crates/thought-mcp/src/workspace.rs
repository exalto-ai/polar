use base64::Engine;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thought_core::{BlockError, Document, Position};
use thought_markdown::{from_markdown, to_markdown_with_spans};
use thought_schema::{Node, Schema, normalize};
use thought_store::{Actor, InitialDocument, Origin, Store};

/// The caller, as asserted by the MCP session. Not authenticated (AD-6) — this
/// is identity so that attribution and per-run revert have something to key on.
#[derive(Debug, Clone)]
pub struct ActorRef {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub model: Option<String>,
    /// Groups one agent turn, so a run can be reverted as a unit (AD-11).
    pub session_id: Option<String>,
}

impl ActorRef {
    pub fn agent(name: &str, model: Option<&str>, session: Option<&str>) -> ActorRef {
        ActorRef {
            // Derived from the client-supplied name, not the connection, so a
            // reconnecting agent stays the same actor instead of fragmenting
            // into one actor per connection.
            id: format!("agent:{name}"),
            kind: "agent".into(),
            display_name: name.into(),
            model: model.map(str::to_string),
            session_id: session.map(str::to_string),
        }
    }

    /// The window's own actor.
    ///
    /// Every window on a device is the same actor, which AD-6 already implies:
    /// identity is per device and per agent, not per window. Two windows are
    /// two *peers* of one human, and the rails say so — which is why the second
    /// human actor cannot appear until M3 puts a second device on the relay.
    pub fn editor() -> ActorRef {
        ActorRef::human("editor")
    }

    pub fn human(name: &str) -> ActorRef {
        ActorRef {
            id: format!("human:{name}"),
            kind: "human".into(),
            display_name: name.into(),
            model: None,
            session_id: None,
        }
    }

    fn origin(&self) -> Origin {
        if self.kind == "agent" {
            Origin::Agent
        } else {
            Origin::Human
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BlockSpan {
    pub block_id: String,
    pub kind: String,
    pub line_start: usize,
    pub line_end: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentView {
    pub doc_id: String,
    pub title: String,
    pub markdown: String,
    pub version: String,
    pub blocks: Vec<BlockSpan>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentSummary {
    pub doc_id: String,
    pub title: String,
    pub updated_at: i64,
    pub word_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub doc_id: String,
    pub title: String,
    pub snippet: String,
}

/// The three parts of a find-and-replace, grouped so the call does not grow an
/// unreadable positional tail.
#[derive(Debug, Clone, Copy)]
pub struct TextEdit<'a> {
    pub find: &'a str,
    pub replace: &'a str,
    /// 1-based. `None` replaces every match.
    pub occurrence: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ActorSummary {
    pub actor_id: String,
    pub kind: String,
    pub display_name: String,
    pub model: Option<String>,
    pub color: String,
    pub last_seen: i64,
    pub edits: i64,
}

/// Who wrote one block. The rails in the window, and the answer to "where did
/// this paragraph come from" for an agent that asks.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BlockAttribution {
    pub block_id: String,
    /// Who first wrote the block, and who last touched it. A paragraph an agent
    /// drafted and a human then reworded is both, and only reporting the latter
    /// loses where the text came from.
    pub created_by: String,
    pub created_at: i64,
    pub touched_by: String,
    pub touched_at: i64,
    pub session_id: Option<String>,
    pub kind: String,
    pub display_name: String,
    pub model: Option<String>,
    pub color: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EditOutcome {
    pub doc_id: String,
    pub block_id: Option<String>,
    pub version: String,
    /// Non-fatal notes — a stale read, a block that moved. See `Workspace`'s
    /// note on why these do not fail the call.
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum WorkspaceError {
    NoSuchDocument(String),
    Block(BlockError),
    InvalidMarkdown(Vec<String>),
    NotFound(String),
    Storage(thought_store::SqlError),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceError::NoSuchDocument(id) => write!(f, "no document `{id}`"),
            WorkspaceError::Block(e) => write!(f, "{e}"),
            WorkspaceError::InvalidMarkdown(errs) => {
                write!(
                    f,
                    "markdown produced an invalid document: {}",
                    errs.join("; ")
                )
            }
            WorkspaceError::NotFound(what) => write!(f, "{what}"),
            WorkspaceError::Storage(e) => write!(f, "storage: {e}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

impl From<thought_store::SqlError> for WorkspaceError {
    fn from(e: thought_store::SqlError) -> Self {
        WorkspaceError::Storage(e)
    }
}

impl From<BlockError> for WorkspaceError {
    fn from(e: BlockError) -> Self {
        WorkspaceError::Block(e)
    }
}

/// Compact updates into a snapshot after this many, per AD-13.
const SNAPSHOT_EVERY: i64 = 200;

/// Store and document cache live under **one** mutex rather than two.
///
/// `rusqlite::Connection` is `Send` but not `Sync`, so the store needs a lock
/// regardless. Two locks would then need a global ordering — and the natural
/// call shapes disagree about it: reading a document goes cache-then-store
/// while creating one goes store-then-cache. One lock removes the question.
/// It also matches M1.4: the daemon owns the document, so there is no
/// concurrency here worth designing around.
struct Inner {
    store: Store,
    docs: HashMap<String, Document>,
    /// Per document, each block's content fingerprint as of the last commit.
    ///
    /// Attribution is a diff, and a diff needs a before. Keeping it in memory
    /// rather than re-deriving it means a commit hashes the tree it has already
    /// built for reindexing, instead of serialising the document a second time.
    prints: HashMap<String, HashMap<String, u64>>,
    /// Deltas committed under the current lock, drained and delivered to the
    /// observer once it is released — user code must not run under our mutex.
    pending: Vec<(String, Vec<u8>, ActorRef)>,
}

/// Notified of every committed change, and by whom.
///
/// The actor matters because agents arrive over MCP, which carries no
/// presence: the only way anyone learns an agent is working on a document is
/// that it wrote something.
type Observer = Arc<dyn Fn(&str, &[u8], &ActorRef) + Send + Sync>;

pub struct Workspace {
    inner: Mutex<Inner>,
    observer: Mutex<Option<Observer>>,
}

impl Workspace {
    pub fn open(path: impl AsRef<Path>) -> Result<Workspace, WorkspaceError> {
        Ok(Workspace {
            inner: Mutex::new(Inner {
                store: Store::open(path)?,
                docs: HashMap::new(),
                prints: HashMap::new(),
                pending: Vec::new(),
            }),
            observer: Mutex::new(None),
        })
    }

    pub fn open_in_memory() -> Result<Workspace, WorkspaceError> {
        Ok(Workspace {
            inner: Mutex::new(Inner {
                store: Store::open_in_memory()?,
                docs: HashMap::new(),
                prints: HashMap::new(),
                pending: Vec::new(),
            }),
            observer: Mutex::new(None),
        })
    }

    /// Watch every committed change, whatever produced it.
    ///
    /// The daemon has one change stream: an agent editing over MCP and a window
    /// typing over the sync socket must both reach every other peer. Wiring the
    /// fan-out to the socket alone left MCP edits invisible to an open editor —
    /// which is most of what makes this app worth building.
    pub fn observe(&self, observer: impl Fn(&str, &[u8], &ActorRef) + Send + Sync + 'static) {
        *self.observer.lock().expect("observer mutex poisoned") = Some(Arc::new(observer));
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let (result, pending) = {
            let mut inner = self.inner.lock().expect("workspace mutex poisoned");
            let result = f(&mut inner);
            (result, std::mem::take(&mut inner.pending))
        };
        // Deliberately outside the lock: an observer that re-entered the
        // workspace would deadlock.
        if !pending.is_empty()
            && let Some(observer) = self
                .observer
                .lock()
                .expect("observer mutex poisoned")
                .clone()
        {
            for (doc_id, delta, actor) in pending {
                observer(&doc_id, &delta, &actor);
            }
        }
        result
    }

    pub fn create_document(
        &self,
        title: &str,
        actor: &ActorRef,
    ) -> Result<DocumentView, WorkspaceError> {
        // Seed the title as a heading rather than storing metadata that the
        // first read would immediately discard. Always leave a paragraph after
        // it so there is somewhere to start typing.
        let mut blocks = Vec::new();
        if !title.trim().is_empty() {
            blocks.push(
                Node::element("heading", vec![Node::text(title.trim(), vec![])])
                    .with_attr("level", 1.into()),
            );
        }
        blocks.push(Node::element("paragraph", vec![]));
        self.create_document_tree(Node::element("doc", blocks), actor)
    }

    /// Import a Markdown snapshot as one new collaborative document.
    ///
    /// Import belongs on creation, before a document id is visible. Building a
    /// blank document and then replacing its first block would expose a
    /// transient seed state, create two attribution entries, and leave an
    /// orphan if parsing failed halfway through.
    pub fn create_document_from_markdown(
        &self,
        _title: &str,
        markdown: &str,
        actor: &ActorRef,
    ) -> Result<DocumentView, WorkspaceError> {
        let mut tree = normalize(&from_markdown(markdown.trim_start_matches('\u{feff}')));
        // ProseMirror requires at least one block. An empty Markdown file maps
        // to the same truly blank document as File > New.
        if tree.content.is_empty() {
            tree.content.push(Node::element("paragraph", vec![]));
        }
        if let Err(errs) = Schema::v0().validate(&tree) {
            return Err(WorkspaceError::InvalidMarkdown(
                errs.iter().map(ToString::to_string).collect(),
            ));
        }
        self.create_document_tree(tree, actor)
    }

    fn create_document_tree(
        &self,
        tree: Node,
        actor: &ActorRef,
    ) -> Result<DocumentView, WorkspaceError> {
        let tree = normalize(&tree);
        if let Err(errs) = Schema::v0().validate(&tree) {
            return Err(WorkspaceError::InvalidMarkdown(
                errs.iter().map(ToString::to_string).collect(),
            ));
        }

        let doc_id = self.with(|inner| -> Result<String, WorkspaceError> {
            inner.register(actor)?;
            let doc_id = uuid::Uuid::now_v7().to_string();
            let doc = Document::new();
            doc.set_document(&tree);
            let state = doc.encode_state();
            let (markdown, _) = to_markdown_with_spans(&tree);
            let title = derive_title(&tree);
            let block_ids = doc
                .blocks()
                .into_iter()
                .map(|block| block.block_id)
                .collect::<Vec<_>>();
            let attributed_at = now_ms();
            inner.store.create_initial_document(InitialDocument {
                id: &doc_id,
                title: &title,
                payload: &state,
                actor_id: &actor.id,
                origin: actor.origin(),
                session_id: actor.session_id.as_deref(),
                markdown: &markdown,
                block_ids: &block_ids,
                attributed_at,
            })?;

            let prints = Inner::fingerprints(&doc);
            inner.docs.insert(doc_id.clone(), doc);
            inner.prints.insert(doc_id.clone(), prints);
            Ok(doc_id)
        })?;
        self.read_document(&doc_id)
    }

    pub fn read_document(&self, doc_id: &str) -> Result<DocumentView, WorkspaceError> {
        self.with(|inner| {
            let doc = inner.doc(doc_id)?;
            let tree = normalize(&doc.read());
            let refs = doc.blocks();
            let version = encode_version(&doc.state_vector());
            let (markdown, spans) = to_markdown_with_spans(&tree);
            let blocks = refs
                .iter()
                .enumerate()
                .map(|(i, r)| BlockSpan {
                    block_id: r.block_id.clone(),
                    kind: r.kind.clone(),
                    line_start: spans.get(i).map(|s| s.0).unwrap_or(0),
                    line_end: spans.get(i).map(|s| s.1).unwrap_or(0),
                })
                .collect();
            Ok(DocumentView {
                doc_id: doc_id.to_string(),
                title: derive_title(&tree),
                markdown,
                version,
                blocks,
            })
        })
    }

    pub fn list_documents(
        &self,
        limit: usize,
        trashed: bool,
    ) -> Result<Vec<DocumentSummary>, WorkspaceError> {
        self.with(|inner| {
            Ok(inner
                .store
                .list_documents(trashed)?
                .into_iter()
                .take(limit)
                .map(|row| DocumentSummary {
                    doc_id: row.id,
                    title: row.title,
                    updated_at: row.updated_at,
                    word_count: 0,
                })
                .collect())
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, WorkspaceError> {
        self.with(|inner| {
            let titles: HashMap<String, String> = inner
                .store
                .list_documents(false)?
                .into_iter()
                .map(|d| (d.id, d.title))
                .collect();
            Ok(inner
                .store
                .search(query, limit)?
                .into_iter()
                .map(|(doc_id, snippet)| SearchHit {
                    title: titles.get(&doc_id).cloned().unwrap_or_default(),
                    doc_id,
                    snippet,
                })
                .collect())
        })
    }

    pub fn replace_block(
        &self,
        doc_id: &str,
        block_id: &str,
        markdown: &str,
        version: Option<&str>,
        actor: &ActorRef,
    ) -> Result<EditOutcome, WorkspaceError> {
        let nodes = parse_blocks(markdown)?;
        let Some(first) = nodes.first() else {
            return Err(WorkspaceError::InvalidMarkdown(vec![
                "markdown produced no blocks".into(),
            ]));
        };
        self.with(|inner| {
            inner.register(actor)?;
            let doc = inner.doc(doc_id)?;
            let before = doc.state_vector();
            let warnings = staleness(doc, version);

            let block = doc.replace_block(block_id, first)?;
            // Extra blocks in the payload follow the one replaced rather than
            // being silently dropped.
            if nodes.len() > 1 {
                doc.insert_blocks(&Position::After(block.block_id.clone()), &nodes[1..])?;
            }
            let version = inner.commit(doc_id, &before, actor)?;
            Ok(EditOutcome {
                doc_id: doc_id.into(),
                block_id: Some(block.block_id),
                version,
                warnings,
            })
        })
    }

    pub fn insert_blocks(
        &self,
        doc_id: &str,
        after: &Position,
        markdown: &str,
        version: Option<&str>,
        actor: &ActorRef,
    ) -> Result<EditOutcome, WorkspaceError> {
        let nodes = parse_blocks(markdown)?;
        self.with(|inner| {
            inner.register(actor)?;
            let doc = inner.doc(doc_id)?;
            let before = doc.state_vector();
            let warnings = staleness(doc, version);
            let created = doc.insert_blocks(after, &nodes)?;
            let version = inner.commit(doc_id, &before, actor)?;
            Ok(EditOutcome {
                doc_id: doc_id.into(),
                block_id: created.first().map(|b| b.block_id.clone()),
                version,
                warnings,
            })
        })
    }

    pub fn delete_block(
        &self,
        doc_id: &str,
        block_id: &str,
        version: Option<&str>,
        actor: &ActorRef,
    ) -> Result<EditOutcome, WorkspaceError> {
        self.with(|inner| {
            inner.register(actor)?;
            let doc = inner.doc(doc_id)?;
            let before = doc.state_vector();
            let warnings = staleness(doc, version);
            doc.delete_block(block_id)?;
            let version = inner.commit(doc_id, &before, actor)?;
            Ok(EditOutcome {
                doc_id: doc_id.into(),
                block_id: None,
                version,
                warnings,
            })
        })
    }

    /// Find and replace within a single block.
    ///
    /// `find` matches the block's **markdown**, not its rendered text, because
    /// markdown is what the agent read — matching against something it never
    /// saw would make failures inexplicable. The consequence is that `find`
    /// must include any emphasis syntax the target carries.
    pub fn replace_text(
        &self,
        doc_id: &str,
        block_id: &str,
        edit: &TextEdit<'_>,
        version: Option<&str>,
        actor: &ActorRef,
    ) -> Result<EditOutcome, WorkspaceError> {
        let TextEdit {
            find,
            replace,
            occurrence,
        } = *edit;
        if find.is_empty() {
            return Err(WorkspaceError::InvalidMarkdown(vec![
                "`find` must not be empty".into(),
            ]));
        }

        let current = self.with(|inner| -> Result<String, WorkspaceError> {
            let doc = inner.doc(doc_id)?;
            let node = doc
                .block(block_id)
                .ok_or_else(|| WorkspaceError::Block(BlockError::NoSuchBlock(block_id.into())))?;
            let (markdown, _) =
                to_markdown_with_spans(&Node::element("doc", vec![normalize(&node)]));
            Ok(markdown)
        })?;

        let hits = current.matches(find).count();
        if hits == 0 {
            return Err(WorkspaceError::NotFound(format!(
                "`{find}` does not appear in block `{block_id}`"
            )));
        }

        let updated = match occurrence {
            // 1-based, matching how a person would say "the second one".
            Some(n) => {
                if n == 0 || n > hits {
                    return Err(WorkspaceError::NotFound(format!(
                        "occurrence {n} of `{find}`; the block has {hits}"
                    )));
                }
                let mut out = String::with_capacity(current.len());
                let mut rest = current.as_str();
                for i in 1..=n {
                    let at = rest.find(find).expect("counted above");
                    out.push_str(&rest[..at]);
                    out.push_str(if i == n { replace } else { find });
                    rest = &rest[at + find.len()..];
                }
                out.push_str(rest);
                out
            }
            None => current.replace(find, replace),
        };

        self.replace_block(doc_id, block_id, &updated, version, actor)
    }

    /// Everything this replica has that the holder of `state_vector` lacks.
    /// The `SUBSCRIBE` half of the sync protocol (§5).
    pub fn sync_since(&self, doc_id: &str, state_vector: &[u8]) -> Result<Vec<u8>, WorkspaceError> {
        self.with(|inner| Ok(inner.doc(doc_id)?.diff_since(state_vector)))
    }

    /// Apply an update frame from a peer — the editor window, or later the
    /// relay.
    ///
    /// Returns `None` when the update changed nothing, so callers can skip
    /// broadcasting and logging a no-op. Yjs updates are idempotent, and a
    /// reconnecting peer resends what it already sent.
    pub fn apply_peer_update(
        &self,
        doc_id: &str,
        update: &[u8],
        actor: &ActorRef,
    ) -> Result<Option<String>, WorkspaceError> {
        self.with(|inner| {
            inner.register(actor)?;

            // Apply to a candidate first. If SQLite refuses the commit, the
            // cached authority must remain at its persisted state. Mutating the
            // cached document in place made a retry look like a no-op, which
            // could then be acknowledged even though the update never reached
            // disk.
            let current_state = inner.doc(doc_id)?.encode_state();
            let candidate = Document::new();
            candidate.apply_update(&current_state).map_err(|e| {
                WorkspaceError::NotFound(format!("could not clone document state: {e}"))
            })?;
            let before = candidate.state_vector();
            candidate
                .apply_update(update)
                .map_err(|e| WorkspaceError::NotFound(format!("bad update: {e}")))?;
            // State vectors track inserted structs, not deletion sets. A
            // deletion-only update can therefore leave the vector unchanged
            // while materially changing the document. Compare complete CRDT
            // state so those updates are persisted and acknowledged too.
            if candidate.encode_state() == current_state {
                return Ok(None);
            }

            let original = inner
                .docs
                .insert(doc_id.to_string(), candidate)
                .expect("document was loaded above");
            let original_prints = inner.prints.get(doc_id).cloned();
            let pending_before = inner.pending.len();

            match inner.commit(doc_id, &before, actor) {
                Ok(version) => Ok(Some(version)),
                Err(error) => {
                    inner.docs.insert(doc_id.to_string(), original);
                    match original_prints {
                        Some(prints) => {
                            inner.prints.insert(doc_id.to_string(), prints);
                        }
                        None => {
                            inner.prints.remove(doc_id);
                        }
                    }
                    inner.pending.truncate(pending_before);
                    Err(error)
                }
            }
        })
    }

    /// Move a document to the trash, or bring it back.
    ///
    /// Soft delete: the tombstone is a field in the document CRDT so it
    /// replicates (AD-14), and editing a tombstoned document does not resurrect
    /// it. The SQLite column is a cache of that field, not the truth.
    pub fn set_document_deleted(
        &self,
        doc_id: &str,
        deleted: bool,
        actor: &ActorRef,
    ) -> Result<EditOutcome, WorkspaceError> {
        self.with(|inner| {
            inner.register(actor)?;
            let doc = inner.doc(doc_id)?;
            let before = doc.state_vector();
            doc.set_deleted_at(deleted.then(now_ms));
            let at = doc.deleted_at();
            let version = inner.commit(doc_id, &before, actor)?;
            inner.store.cache_deleted_at(doc_id, at)?;
            Ok(EditOutcome {
                doc_id: doc_id.into(),
                block_id: None,
                version,
                warnings: vec![],
            })
        })
    }

    /// Who has worked on this document. Powers the window's connections panel,
    /// and is the first visible use of the attribution AD-6 insisted on keeping
    /// from the first commit.
    pub fn document_actors(&self, doc_id: &str) -> Result<Vec<ActorSummary>, WorkspaceError> {
        self.with(|inner| {
            Ok(inner
                .store
                .actors_for_document(doc_id)?
                .into_iter()
                .map(|a| ActorSummary {
                    actor_id: a.actor_id,
                    kind: a.kind,
                    display_name: a.display_name,
                    model: a.model,
                    color: a.color,
                    last_seen: a.last_seen,
                    edits: a.edits,
                })
                .collect())
        })
    }

    /// Who wrote each block of a document.
    ///
    /// Answers the per-block question the op log can only answer per *update*.
    /// The table behind this is derived state (like the FTS index): a document
    /// whose provenance has never been computed is attributed by replaying its
    /// log here, once, and read from the table forever after.
    ///
    /// A document that arrives with content but no log — everything M3's relay
    /// will deliver — reports nothing, which is the honest answer. Blank means
    /// unknown, never "mine".
    pub fn block_provenance(&self, doc_id: &str) -> Result<Vec<BlockAttribution>, WorkspaceError> {
        self.with(|inner| {
            // Hydrating also backfills, so a document that predates this table
            // is attributed on first read rather than staying blank forever.
            inner.doc(doc_id)?;
            Ok(inner
                .store
                .provenance_for_document(doc_id)?
                .into_iter()
                .map(|a| BlockAttribution {
                    block_id: a.block_id,
                    created_by: a.created_by,
                    created_at: a.created_at,
                    touched_by: a.touched_by,
                    touched_at: a.touched_at,
                    session_id: a.session_id,
                    kind: a.kind,
                    display_name: a.display_name,
                    model: a.model,
                    color: a.color,
                })
                .collect())
        })
    }

    /// Attribution for the whole log — the activity feed and per-run revert.
    pub fn attribution(
        &self,
        doc_id: &str,
    ) -> Result<Vec<(String, Option<String>)>, WorkspaceError> {
        self.with(|inner| {
            Ok(inner
                .store
                .log(doc_id)?
                .into_iter()
                .map(|u| (u.actor_id, u.session_id))
                .collect())
        })
    }
}

impl Inner {
    fn register(&self, actor: &ActorRef) -> Result<(), WorkspaceError> {
        self.store.upsert_actor(&Actor {
            id: actor.id.clone(),
            kind: actor.kind.clone(),
            display_name: actor.display_name.clone(),
            model: actor.model.clone(),
            color: color_for(&actor.id),
        })?;
        Ok(())
    }

    /// Documents are hydrated on first touch, not at boot.
    fn doc(&mut self, doc_id: &str) -> Result<&Document, WorkspaceError> {
        if !self.docs.contains_key(doc_id) {
            let restored = self.store.restore(doc_id)?;
            if restored.snapshot.is_none() && restored.updates.is_empty() {
                return Err(WorkspaceError::NoSuchDocument(doc_id.to_string()));
            }
            let doc = Document::new();
            if let Some(state) = &restored.snapshot {
                doc.apply_update(state).map_err(|e| {
                    WorkspaceError::InvalidMarkdown(vec![format!("corrupt snapshot: {e}")])
                })?;
            }
            for update in &restored.updates {
                doc.apply_update(update).map_err(|e| {
                    WorkspaceError::InvalidMarkdown(vec![format!("corrupt update: {e}")])
                })?;
            }
            self.docs.insert(doc_id.to_string(), doc);

            // Seed the diff baseline from the document as it stands, so the
            // next commit compares against reality rather than an empty map
            // and re-attributes every block to whoever typed next.
            let prints = Self::fingerprints(self.docs.get(doc_id).expect("just inserted"));
            self.prints.insert(doc_id.to_string(), prints);

            // Documents written before this table existed have a full log and
            // no attribution. Replay pays that off once.
            if !self.store.has_provenance(doc_id)? {
                self.backfill(doc_id)?;
            }
        }
        Ok(self.docs.get(doc_id).expect("just inserted"))
    }

    /// Each top-level block's content, hashed.
    ///
    /// Block ids are intrinsic and stable (AD-5), so a block that changed keeps
    /// its id and only its fingerprint moves — which is exactly the signal
    /// attribution needs, and why this is a hash rather than an id comparison.
    fn fingerprints(doc: &Document) -> HashMap<String, u64> {
        let tree = normalize(&doc.read());
        let refs = doc.blocks();
        // `normalize` only merges and drops *text* nodes, and a document's
        // top-level children are always elements, so these stay in step — the
        // same pairing `read_document` makes to attach line spans.
        refs.iter()
            .zip(tree.content.iter())
            .map(|(r, node)| (r.block_id.clone(), fingerprint(node)))
            .collect()
    }

    /// Attribute whatever this commit changed to the actor that wrote it.
    ///
    /// A block is *created* the first time its id appears and *touched* when
    /// its fingerprint moves. Blocks that did not change are left alone, so a
    /// one-word edit does not re-attribute the whole document to whoever made
    /// it.
    fn attribute(&mut self, doc_id: &str, actor: &ActorRef, at: i64) -> Result<(), WorkspaceError> {
        let doc = self.docs.get(doc_id).expect("document is loaded");
        let current = Self::fingerprints(doc);
        let previous = self.prints.get(doc_id);

        for (block_id, print) in &current {
            let unchanged = previous.and_then(|p| p.get(block_id)) == Some(print);
            if unchanged {
                continue;
            }
            self.store
                .touch_block(doc_id, block_id, &actor.id, actor.session_id.as_deref(), at)?;
        }

        if previous.is_some_and(|p| p.keys().any(|id| !current.contains_key(id))) {
            let keep: HashSet<String> = current.keys().cloned().collect();
            self.store.forget_blocks(doc_id, &keep)?;
        }
        self.prints.insert(doc_id.to_string(), current);
        Ok(())
    }

    /// Attribute a document that has never been attributed, by replaying its
    /// log one update at a time.
    ///
    /// Deliberately replays the *log* rather than starting from a snapshot:
    /// snapshots compact history away and the log never does (AD-13), so the
    /// log is the only thing that can say who wrote what. Runs once per
    /// document — every commit after this one attributes itself incrementally.
    fn backfill(&mut self, doc_id: &str) -> Result<(), WorkspaceError> {
        let log = self.store.log(doc_id)?;
        let replay = Document::new();
        let mut previous: HashMap<String, u64> = HashMap::new();

        for entry in &log {
            // A corrupt frame stops the backfill rather than failing the read:
            // partial attribution is worth more than none, and the document
            // itself hydrates from the same bytes through its own path.
            if replay.apply_update(&entry.payload).is_err() {
                break;
            }
            let current = Self::fingerprints(&replay);
            for (block_id, print) in &current {
                if previous.get(block_id) == Some(print) {
                    continue;
                }
                self.store.touch_block(
                    doc_id,
                    block_id,
                    &entry.actor_id,
                    entry.session_id.as_deref(),
                    entry.created_at,
                )?;
            }
            previous = current;
        }

        // Blocks that existed partway through the log but not at the end.
        let keep: HashSet<String> = previous.keys().cloned().collect();
        self.store.forget_blocks(doc_id, &keep)?;
        Ok(())
    }

    /// Persist everything written since `before`, then keep derived state in
    /// step.
    fn commit(
        &mut self,
        doc_id: &str,
        before: &[u8],
        actor: &ActorRef,
    ) -> Result<String, WorkspaceError> {
        let doc = self.docs.get(doc_id).expect("document is loaded");
        let delta = doc.diff_since(before);
        let tree = normalize(&doc.read());
        let state = doc.encode_state();
        let state_vector = doc.state_vector();

        let seq = self.store.append_update(
            doc_id,
            &delta,
            &actor.id,
            actor.origin(),
            actor.session_id.as_deref(),
        )?;
        self.pending
            .push((doc_id.to_string(), delta.clone(), actor.clone()));

        // Before reindexing, while the actor is still in hand: which blocks
        // this commit changed, and who to credit for them.
        self.attribute(doc_id, actor, now_ms())?;

        let (markdown, _) = to_markdown_with_spans(&tree);
        // Reindexed on every mutation, not on snapshot as M1.4 first said.
        // Serializing a document and writing two rows is cheap; agents reading
        // a stale index is not, and search is how they avoid reading every
        // document.
        self.store
            .reindex(doc_id, &derive_title(&tree), &markdown)?;

        if self.store.updates_since_snapshot(doc_id)? >= SNAPSHOT_EVERY {
            self.store
                .write_snapshot(doc_id, seq, &state, &state_vector)?;
        }
        Ok(encode_version(&state_vector))
    }
}

fn parse_blocks(markdown: &str) -> Result<Vec<Node>, WorkspaceError> {
    let parsed = normalize(&from_markdown(markdown));
    if let Err(errs) = Schema::v0().validate(&parsed) {
        return Err(WorkspaceError::InvalidMarkdown(
            errs.iter().map(ToString::to_string).collect(),
        ));
    }
    Ok(parsed.content)
}

/// A stale `version` **warns and proceeds**.
///
/// The CRDT merges correctly regardless, so a stale read is not a conflict — it
/// is the semantic risk that the agent reasoned about text which has since
/// moved. Failing would punish the agent for something that is not an error and
/// push callers toward re-reading whole documents, which is exactly the traffic
/// AD-5 exists to prevent.
fn staleness(doc: &Document, version: Option<&str>) -> Vec<String> {
    let Some(version) = version else {
        return vec![];
    };
    let Some(seen) = decode_version(version) else {
        return vec!["unreadable version token; proceeding".into()];
    };
    if doc.diff_since(&seen).len() > 2 {
        vec!["document changed since you read it; edit applied to the current state".into()]
    } else {
        vec![]
    }
}

/// A block's content, hashed to a value that changes when the block does.
///
/// Walks the node rather than serialising it: attribution runs on every commit,
/// and building a JSON string per block per keystroke would make writing pay
/// for a feature that only reading uses.
fn fingerprint(node: &Node) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_node(node, &mut hasher);
    hasher.finish()
}

fn hash_node(node: &Node, hasher: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    node.kind.hash(hasher);
    node.text.hash(hasher);
    for (key, value) in &node.attrs {
        key.hash(hasher);
        // `serde_json::Value` is not `Hash`, and its `Display` is canonical
        // enough for a change detector.
        value.to_string().hash(hasher);
    }
    for mark in &node.marks {
        mark.kind.hash(hasher);
        for (key, value) in &mark.attrs {
            key.hash(hasher);
            value.to_string().hash(hasher);
        }
    }
    for child in &node.content {
        hash_node(child, hasher);
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn derive_title(tree: &Node) -> String {
    tree.content
        .iter()
        .find(|n| n.kind == "heading")
        .or_else(|| tree.content.iter().find(|n| !n.content.is_empty()))
        .map(|n| {
            let text: String = n.content.iter().filter_map(|c| c.text.clone()).collect();
            text.chars().take(120).collect()
        })
        .filter(|t: &String| !t.trim().is_empty())
        .unwrap_or_else(|| "Untitled".into())
}

fn color_for(actor_id: &str) -> String {
    const PALETTE: &[&str] = &["#4c8dff", "#e0a44a", "#b98cff", "#5ac88f", "#ff7a6b"];
    let sum: usize = actor_id.bytes().map(|b| b as usize).sum();
    PALETTE[sum % PALETTE.len()].to_string()
}

fn encode_version(state_vector: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(state_vector)
}

fn decode_version(version: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(version)
        .ok()
}
