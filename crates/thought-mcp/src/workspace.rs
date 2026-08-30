use base64::Engine;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thought_core::{BlockError, Document, Position};
use thought_markdown::{from_markdown, to_markdown_with_spans};
use thought_provenance::{
    Alignment, Assurance, CurrentSourceSummary, Ingress, LineageError, LineageState,
    LiveLineageSpan, SemanticRange, SourceDescriptor, SourceId, TextLocation,
};
use thought_schema::{Node, Schema, normalize};
use thought_store::{
    Actor, InitialDocument, LineageSpanRow, LineageUpdate, Origin, ProvenanceEventInput,
    ProvenanceEventRow, Store,
};

use crate::lineage::{ProseMirrorRange, SnapshotError, block_snapshots, semantic_ranges};
use crate::mutation::MutationContext;

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

    pub fn reviewer(
        connection_id: &str,
        display_name: &str,
        model: Option<&str>,
        session: Option<&str>,
    ) -> ActorRef {
        ActorRef {
            id: format!("reviewer:{connection_id}"),
            kind: "agent".into(),
            display_name: display_name.into(),
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentLineage {
    pub doc_id: String,
    /// Digest of the normalized wording and formatting represented by this response.
    pub current_wording_revision: String,
    pub summary: CurrentSourceSummary,
    pub spans: Vec<LiveLineageSpan>,
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
    Snapshot(SnapshotError),
    Lineage(LineageError),
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
            WorkspaceError::Snapshot(e) => write!(f, "snapshot: {e}"),
            WorkspaceError::Lineage(e) => write!(f, "lineage: {e}"),
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

impl From<SnapshotError> for WorkspaceError {
    fn from(error: SnapshotError) -> Self {
        Self::Snapshot(error)
    }
}

impl From<LineageError> for WorkspaceError {
    fn from(error: LineageError) -> Self {
        Self::Lineage(error)
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
    lineages: HashMap<String, LineageState>,
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
                lineages: HashMap::new(),
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
                lineages: HashMap::new(),
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

    pub(crate) fn with_store<T>(&self, f: impl FnOnce(&Store) -> T) -> T {
        self.with(|inner| f(&inner.store))
    }

    pub fn create_document(
        &self,
        title: &str,
        actor: &ActorRef,
    ) -> Result<DocumentView, WorkspaceError> {
        self.create_document_with_context(title, actor, &default_context(actor))
    }

    pub fn create_document_with_context(
        &self,
        title: &str,
        actor: &ActorRef,
        context: &MutationContext,
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
        self.create_document_tree(Node::element("doc", blocks), actor, context)
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
        self.create_document_from_markdown_with_context(
            _title,
            markdown,
            actor,
            &MutationContext::imported(),
        )
    }

    pub fn create_document_from_markdown_with_context(
        &self,
        _title: &str,
        markdown: &str,
        actor: &ActorRef,
        context: &MutationContext,
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
        self.create_document_tree(tree, actor, context)
    }

    fn create_document_tree(
        &self,
        tree: Node,
        actor: &ActorRef,
        context: &MutationContext,
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
            let event_id = inner.store.next_update_seq()?;
            let snapshots = block_snapshots(&doc, &tree)?;
            let mut source = context.source(SourceId(event_id as u64));
            source.alignment = Alignment::Exact;
            let lineage = LineageState::seed(snapshots, source.clone())?;
            let spans = spans_to_store(lineage.spans())?;
            let event = event_input(event_id, actor, &source, "edit", attributed_at);
            inner.store.create_initial_document_with_lineage(
                InitialDocument {
                    id: &doc_id,
                    title: &title,
                    payload: &state,
                    actor_id: &actor.id,
                    origin: actor.origin(),
                    session_id: actor.session_id.as_deref(),
                    markdown: &markdown,
                    block_ids: &block_ids,
                    attributed_at,
                },
                event_id,
                event,
                &spans,
            )?;

            let prints = Inner::fingerprints(&doc);
            inner.docs.insert(doc_id.clone(), doc);
            inner.prints.insert(doc_id.clone(), prints);
            inner.lineages.insert(doc_id.clone(), lineage);
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

    pub fn list_documents_scoped(
        &self,
        limit: usize,
        trashed: bool,
        document_id: Option<&str>,
    ) -> Result<Vec<DocumentSummary>, WorkspaceError> {
        let mut documents = self.list_documents(usize::MAX, trashed)?;
        if let Some(document_id) = document_id {
            documents.retain(|document| document.doc_id == document_id);
        }
        documents.truncate(limit);
        Ok(documents)
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

    pub fn search_scoped(
        &self,
        query: &str,
        limit: usize,
        document_id: Option<&str>,
    ) -> Result<Vec<SearchHit>, WorkspaceError> {
        let Some(document_id) = document_id else {
            return self.search(query, limit);
        };
        self.with(|inner| {
            let title = inner
                .store
                .list_documents(false)?
                .into_iter()
                .find(|document| document.id == document_id)
                .map(|document| document.title)
                .unwrap_or_default();
            Ok(inner
                .store
                .search_document(query, document_id, limit)?
                .into_iter()
                .map(|(doc_id, snippet)| SearchHit {
                    title: title.clone(),
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
        self.replace_block_with_context(
            doc_id,
            block_id,
            markdown,
            version,
            actor,
            &default_context(actor),
        )
    }

    pub fn replace_block_with_context(
        &self,
        doc_id: &str,
        block_id: &str,
        markdown: &str,
        version: Option<&str>,
        actor: &ActorRef,
        context: &MutationContext,
    ) -> Result<EditOutcome, WorkspaceError> {
        let nodes = parse_blocks(markdown)?;
        let Some(first) = nodes.first() else {
            return Err(WorkspaceError::InvalidMarkdown(vec![
                "markdown produced no blocks".into(),
            ]));
        };
        self.with(|inner| {
            let ((block, warnings), version) = inner.mutate(doc_id, actor, context, |doc| {
                let warnings = staleness(doc, version);
                let block = doc.replace_block(block_id, first)?;
                if nodes.len() > 1 {
                    doc.insert_blocks(&Position::After(block.block_id.clone()), &nodes[1..])?;
                }
                Ok((block, warnings))
            })?;
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
        self.insert_blocks_with_context(
            doc_id,
            after,
            markdown,
            version,
            actor,
            &default_context(actor),
        )
    }

    pub fn insert_blocks_with_context(
        &self,
        doc_id: &str,
        after: &Position,
        markdown: &str,
        version: Option<&str>,
        actor: &ActorRef,
        context: &MutationContext,
    ) -> Result<EditOutcome, WorkspaceError> {
        let nodes = parse_blocks(markdown)?;
        self.with(|inner| {
            let ((created, warnings), version) = inner.mutate(doc_id, actor, context, |doc| {
                let warnings = staleness(doc, version);
                let created = doc.insert_blocks(after, &nodes)?;
                Ok((created, warnings))
            })?;
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
        self.delete_block_with_context(
            doc_id,
            block_id,
            version,
            actor,
            &MutationContext::command(),
        )
    }

    pub fn delete_block_with_context(
        &self,
        doc_id: &str,
        block_id: &str,
        version: Option<&str>,
        actor: &ActorRef,
        context: &MutationContext,
    ) -> Result<EditOutcome, WorkspaceError> {
        self.with(|inner| {
            let (warnings, version) = inner.mutate(doc_id, actor, context, |doc| {
                let warnings = staleness(doc, version);
                doc.delete_block(block_id)?;
                Ok(warnings)
            })?;
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
        self.replace_text_with_context(
            doc_id,
            block_id,
            edit,
            version,
            actor,
            &default_context(actor),
        )
    }

    pub fn replace_text_with_context(
        &self,
        doc_id: &str,
        block_id: &str,
        edit: &TextEdit<'_>,
        version: Option<&str>,
        actor: &ActorRef,
        context: &MutationContext,
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

        self.replace_block_with_context(doc_id, block_id, &updated, version, actor, context)
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
        self.apply_peer_update_with_context(doc_id, update, actor, &MutationContext::unknown())
    }

    pub fn apply_peer_update_with_context(
        &self,
        doc_id: &str,
        update: &[u8],
        actor: &ActorRef,
        context: &MutationContext,
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

            inner
                .commit_candidate(doc_id, candidate, actor, context, None)
                .map(Some)
        })
    }

    /// Apply one complete local editor dispatch with its before/after ranges.
    /// Invalid or incomplete ranges never reject the edit; they only lower its
    /// alignment from exact to inferred.
    pub fn apply_editor_update(
        &self,
        doc_id: &str,
        update: &[u8],
        ranges: &[ProseMirrorRange],
    ) -> Result<Option<String>, WorkspaceError> {
        self.with(|inner| {
            let actor = ActorRef::editor();
            inner.register(&actor)?;
            let (current_state, before_tree) = {
                let current = inner.doc(doc_id)?;
                (current.encode_state(), normalize(&current.read()))
            };
            let candidate = Document::new();
            candidate.apply_update(&current_state).map_err(|error| {
                WorkspaceError::NotFound(format!("could not clone document state: {error}"))
            })?;
            candidate
                .apply_update(update)
                .map_err(|error| WorkspaceError::NotFound(format!("bad update: {error}")))?;
            if candidate.encode_state() == current_state {
                return Ok(None);
            }

            let after_tree = normalize(&candidate.read());
            let exact = if ranges.is_empty() {
                None
            } else {
                semantic_ranges(&before_tree, &after_tree, ranges).ok()
            };
            inner
                .commit_candidate(
                    doc_id,
                    candidate,
                    &actor,
                    &MutationContext::entered(),
                    exact.as_deref(),
                )
                .map(Some)
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
        self.set_document_deleted_with_context(doc_id, deleted, actor, &MutationContext::command())
    }

    pub fn set_document_deleted_with_context(
        &self,
        doc_id: &str,
        deleted: bool,
        actor: &ActorRef,
        context: &MutationContext,
    ) -> Result<EditOutcome, WorkspaceError> {
        self.with(|inner| {
            let (_, version) = inner.mutate(doc_id, actor, context, |doc| {
                doc.set_deleted_at(deleted.then(now_ms));
                Ok(())
            })?;
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

    pub fn document_lineage(&self, doc_id: &str) -> Result<DocumentLineage, WorkspaceError> {
        self.with(|inner| {
            let current_wording_revision =
                thought_markdown::current_wording_revision(&inner.doc(doc_id)?.read());
            let lineage = inner
                .lineages
                .get(doc_id)
                .expect("document hydration installs lineage");
            Ok(DocumentLineage {
                doc_id: doc_id.to_string(),
                current_wording_revision,
                summary: lineage.current_source_summary()?,
                spans: lineage.spans().to_vec(),
            })
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
            let tree = normalize(&doc.read());
            let snapshots = block_snapshots(&doc, &tree)?;
            let events = self.store.provenance_events(doc_id)?;
            let lineage = if events.is_empty() {
                let event_id = self
                    .store
                    .latest_update_seq(doc_id)?
                    .ok_or_else(|| WorkspaceError::NoSuchDocument(doc_id.to_string()))?;
                let context = MutationContext::legacy_unknown();
                let source = context.source(SourceId(event_id as u64));
                let lineage = LineageState::seed(snapshots, source.clone())?;
                let spans = spans_to_store(lineage.spans())?;
                let at = now_ms();
                let event = ProvenanceEventInput {
                    event_id,
                    actor_id: None,
                    action: "edit",
                    group_key: context.group_key(),
                    source_label: context.source_label(),
                    ingress: context.ingress().as_str(),
                    assurance: context.assurance().as_str(),
                    alignment: context.alignment().as_str(),
                    session_id: None,
                    created_at: at,
                };
                self.store
                    .seed_legacy_lineage(doc_id, event_id, event, &spans)?;
                lineage
            } else {
                let sources = events
                    .into_iter()
                    .map(source_from_row)
                    .collect::<Result<BTreeMap<_, _>, _>>()?;
                let spans = self
                    .store
                    .lineage_spans(doc_id)?
                    .into_iter()
                    .map(span_from_row)
                    .collect::<Result<Vec<_>, _>>()?;
                LineageState::from_parts(snapshots, spans, sources)?
            };
            let prints = Self::fingerprints(&doc);
            self.docs.insert(doc_id.to_string(), doc);
            self.prints.insert(doc_id.to_string(), prints);
            self.lineages.insert(doc_id.to_string(), lineage);
            if !self.store.has_provenance(doc_id)? {
                self.backfill_block_provenance(doc_id)?;
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

    fn backfill_block_provenance(&mut self, doc_id: &str) -> Result<(), WorkspaceError> {
        let replay = Document::new();
        let mut previous = HashMap::new();
        for entry in self.store.log(doc_id)? {
            if replay.apply_update(&entry.payload).is_err() {
                break;
            }
            let current = Self::fingerprints(&replay);
            for (block_id, fingerprint) in &current {
                if previous.get(block_id) != Some(fingerprint) {
                    self.store.touch_block(
                        doc_id,
                        block_id,
                        &entry.actor_id,
                        entry.session_id.as_deref(),
                        entry.created_at,
                    )?;
                }
            }
            previous = current;
        }
        self.store
            .forget_blocks(doc_id, &previous.keys().cloned().collect::<HashSet<_>>())?;
        Ok(())
    }

    fn mutate<T>(
        &mut self,
        doc_id: &str,
        actor: &ActorRef,
        context: &MutationContext,
        operation: impl FnOnce(&Document) -> Result<T, WorkspaceError>,
    ) -> Result<(T, String), WorkspaceError> {
        self.register(actor)?;
        let state = self.doc(doc_id)?.encode_state();
        let candidate = Document::new();
        candidate.apply_update(&state).map_err(|error| {
            WorkspaceError::NotFound(format!("could not clone document state: {error}"))
        })?;
        let value = operation(&candidate)?;
        let version = self.commit_candidate(doc_id, candidate, actor, context, None)?;
        Ok((value, version))
    }

    fn commit_candidate(
        &mut self,
        doc_id: &str,
        candidate: Document,
        actor: &ActorRef,
        context: &MutationContext,
        exact_ranges: Option<&[SemanticRange]>,
    ) -> Result<String, WorkspaceError> {
        let current = self.docs.get(doc_id).expect("document is loaded");
        let delta = candidate.diff_since(&current.state_vector());
        let tree = normalize(&candidate.read());
        let snapshots = block_snapshots(&candidate, &tree)?;
        let event_id = self.store.next_update_seq()?;
        let previous_lineage = self
            .lineages
            .get(doc_id)
            .expect("document hydration installs lineage");
        let mut source = context.source(SourceId(event_id as u64));
        let lineage = if let Some(ranges) = exact_ranges {
            source.alignment = Alignment::Exact;
            match previous_lineage.reconcile_exact(snapshots.clone(), source.clone(), ranges) {
                Ok(lineage) => lineage,
                Err(LineageError::InvalidRange | LineageError::RangeMismatch) => {
                    source.alignment = Alignment::Inferred;
                    previous_lineage.reconcile(snapshots, source.clone())?
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            previous_lineage.reconcile(snapshots, source.clone())?
        };
        let spans = spans_to_store(lineage.spans())?;
        let current_prints = Self::fingerprints(&candidate);
        let previous_prints = self.prints.get(doc_id).expect("document is loaded");
        let touched_blocks = current_prints
            .iter()
            .filter(|(block_id, print)| previous_prints.get(*block_id) != Some(*print))
            .map(|(block_id, _)| block_id.clone())
            .collect::<Vec<_>>();
        let current_blocks = current_prints.keys().cloned().collect::<Vec<_>>();
        let at = now_ms();
        let action = if current.deleted_at() == candidate.deleted_at() {
            "edit"
        } else if candidate.deleted_at().is_some() {
            "trash"
        } else {
            "restore"
        };
        let event = event_input(event_id, actor, &source, action, at);
        let (markdown, _) = to_markdown_with_spans(&tree);
        let title = derive_title(&tree);
        self.store.commit_lineage_update(LineageUpdate {
            doc_id,
            expected_seq: event_id,
            payload: &delta,
            actor_id: &actor.id,
            origin: actor.origin(),
            session_id: actor.session_id.as_deref(),
            title: &title,
            markdown: &markdown,
            deleted_at: candidate.deleted_at(),
            touched_blocks: &touched_blocks,
            current_blocks: &current_blocks,
            event,
            spans: &spans,
        })?;

        let state = candidate.encode_state();
        let state_vector = candidate.state_vector();
        self.docs.insert(doc_id.to_string(), candidate);
        self.prints.insert(doc_id.to_string(), current_prints);
        self.lineages.insert(doc_id.to_string(), lineage);
        self.pending
            .push((doc_id.to_string(), delta, actor.clone()));
        if self.store.updates_since_snapshot(doc_id)? >= SNAPSHOT_EVERY {
            let _ = self
                .store
                .write_snapshot(doc_id, event_id, &state, &state_vector);
        }
        Ok(encode_version(&state_vector))
    }
}

fn default_context(actor: &ActorRef) -> MutationContext {
    if actor.kind == "agent" {
        MutationContext::mcp(actor.display_name.clone())
    } else {
        MutationContext::entered()
    }
}

fn event_input<'a>(
    event_id: i64,
    actor: &'a ActorRef,
    source: &'a SourceDescriptor,
    action: &'a str,
    created_at: i64,
) -> ProvenanceEventInput<'a> {
    ProvenanceEventInput {
        event_id,
        actor_id: Some(&actor.id),
        action,
        group_key: &source.group_key,
        source_label: &source.label,
        ingress: source.ingress.as_str(),
        assurance: source.assurance.as_str(),
        alignment: source.alignment.as_str(),
        session_id: actor.session_id.as_deref(),
        created_at,
    }
}

fn spans_to_store(spans: &[LiveLineageSpan]) -> Result<Vec<LineageSpanRow>, WorkspaceError> {
    spans
        .iter()
        .map(|span| {
            Ok(LineageSpanRow {
                block_id: span.location.block_id.clone(),
                node_path: serde_json::to_string(&span.location.path).map_err(|error| {
                    WorkspaceError::NotFound(format!("could not encode lineage path: {error}"))
                })?,
                start_utf16: i64::from(span.location.from_utf16),
                end_utf16: i64::from(span.location.to_utf16),
                source_event_id: i64::try_from(span.source_id.0).map_err(|_| {
                    WorkspaceError::NotFound("lineage source id exceeds SQLite range".into())
                })?,
            })
        })
        .collect()
}

fn span_from_row(row: LineageSpanRow) -> Result<LiveLineageSpan, WorkspaceError> {
    Ok(LiveLineageSpan {
        location: TextLocation {
            block_id: row.block_id,
            path: serde_json::from_str(&row.node_path).map_err(|error| {
                WorkspaceError::NotFound(format!("invalid stored lineage path: {error}"))
            })?,
            from_utf16: u32::try_from(row.start_utf16)
                .map_err(|_| WorkspaceError::NotFound("invalid stored lineage start".into()))?,
            to_utf16: u32::try_from(row.end_utf16)
                .map_err(|_| WorkspaceError::NotFound("invalid stored lineage end".into()))?,
        },
        source_id: SourceId(
            u64::try_from(row.source_event_id)
                .map_err(|_| WorkspaceError::NotFound("invalid stored lineage source".into()))?,
        ),
    })
}

fn source_from_row(
    row: ProvenanceEventRow,
) -> Result<(SourceId, SourceDescriptor), WorkspaceError> {
    let id = SourceId(
        u64::try_from(row.event_id)
            .map_err(|_| WorkspaceError::NotFound("invalid provenance event id".into()))?,
    );
    let ingress = Ingress::parse(&row.ingress)
        .ok_or_else(|| WorkspaceError::NotFound("invalid provenance ingress".into()))?;
    let assurance = Assurance::parse(&row.assurance)
        .ok_or_else(|| WorkspaceError::NotFound("invalid provenance assurance".into()))?;
    let alignment = Alignment::parse(&row.alignment)
        .ok_or_else(|| WorkspaceError::NotFound("invalid provenance alignment".into()))?;
    Ok((
        id,
        SourceDescriptor::new(
            id,
            row.group_key,
            row.source_label,
            ingress,
            assurance,
            alignment,
        ),
    ))
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
