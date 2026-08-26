use crate::lineage::{SnapshotError, block_snapshots};
use crate::mutation::{MutationContext, action_name, assurance_name, ingress_name};
use crate::provenance_hash::{
    ActorEventMetadata, CURRENT_EVENT_CHAIN_VERSION, EventAction, EventHashInput, EventReferences,
    LineageHashInput, UpdateLogEntry, document_digest, empty_update_log_digest, event_chain_digest,
    live_lineage_digest, update_log_digest,
};
use crate::provenance_store::{
    ProvenanceStoreError, deltas_from_store, deltas_to_store, lineage_from_store, spans_to_store,
};
use base64::Engine;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thought_core::{BlockError, Document, Position};
use thought_markdown::{from_markdown, to_markdown_with_spans};
use thought_provenance::{
    Assurance, CurrentSourceSummary, Ingress, LineageState, ReconcileError, SourceId,
};
use thought_schema::{Node, Schema, normalize};
use thought_store::{
    Actor, BlockTouchInput, InitialProvenanceDocumentInput, LineageRebuildInput, Origin,
    ProvenanceCommitInput, ProvenanceEventInput, ProvenanceRecordInput, ProvenanceUpdateInput,
    ReadyLineageInput, Store,
};

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

/// Current wording contribution under the versioned alignment algorithm.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentLineage {
    pub algorithm_version: u32,
    /// V1 has no transaction-range anchors. Equal text is preserved through a
    /// deterministic semantic alignment, but duplicate occurrences with
    /// different sources can be observationally ambiguous.
    pub alignment: &'static str,
    pub summary: CurrentSourceSummary,
    pub spans: Vec<thought_provenance::LiveLineageSpan>,
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
    Snapshot(SnapshotError),
    Reconcile(ReconcileError),
    ProvenanceStore(ProvenanceStoreError),
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
            WorkspaceError::Snapshot(e) => write!(f, "provenance snapshot: {e}"),
            WorkspaceError::Reconcile(e) => write!(f, "provenance reconciliation: {e}"),
            WorkspaceError::ProvenanceStore(e) => write!(f, "persisted provenance: {e}"),
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
    fn from(e: SnapshotError) -> Self {
        WorkspaceError::Snapshot(e)
    }
}

impl From<ReconcileError> for WorkspaceError {
    fn from(e: ReconcileError) -> Self {
        WorkspaceError::Reconcile(e)
    }
}

impl From<ProvenanceStoreError> for WorkspaceError {
    fn from(e: ProvenanceStoreError) -> Self {
        WorkspaceError::ProvenanceStore(e)
    }
}

/// Compact updates into a snapshot after this many, per AD-13.
const SNAPSHOT_EVERY: i64 = 200;

/// Frozen V1 semantic reconciliation version.
///
/// Do not increment this independently. A future evidence-suite version needs
/// a schema migration plus version-dispatched verification and reconciliation
/// for already-recorded events before this value can change safely.
const LINEAGE_ALGORITHM_VERSION: u32 = 1;

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
    /// Current semantic lineage, hydrated and verified with each document.
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

    pub fn create_document(
        &self,
        title: &str,
        actor: &ActorRef,
    ) -> Result<DocumentView, WorkspaceError> {
        let context = default_context(actor, MutationContext::entered());
        self.create_document_with_context(title, actor, &context)
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
        let context = default_context(actor, MutationContext::imported());
        self.create_document_from_markdown_with_context(_title, markdown, actor, &context)
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
            let doc_id = uuid::Uuid::now_v7().to_string();
            let doc = Document::new();
            doc.set_document(&tree);
            let state = doc.encode_state();
            let (markdown, _) = to_markdown_with_spans(&tree);
            let title = derive_title(&tree);
            let snapshots = block_snapshots(&doc, &tree)?;
            let event_id = inner.store.next_provenance_event_id()?;
            let update_seq = inner.store.next_update_seq()?;
            let source_id = source_id(event_id)?;
            let source = context.source(source_id);
            let empty = LineageState::seed(vec![], source.clone())?;
            let reconciled = empty.reconcile(snapshots.clone(), source)?;
            let at = now_ms();
            let update_log_root =
                extend_update_log_root(None, &doc_id, update_seq, &state, actor, at)?;
            let event = event_input(EventBuild {
                event_id,
                update_seq: Some(update_seq),
                doc_id: &doc_id,
                actor: Some(actor),
                context,
                action: context.action(),
                before_hash: document_digest(&[], None),
                after_hash: document_digest(&snapshots, doc.deleted_at()),
                update_log_root,
                previous_hash: None,
                deltas: &reconciled.deltas,
                created_at: at,
                recorded_at: at,
            })?;
            let spans = spans_to_store(reconciled.state.spans())?;
            let lineage =
                ready_lineage(&doc_id, update_seq, event_id, reconciled.state.spans(), at)?;
            let block_ids = doc
                .blocks()
                .into_iter()
                .map(|block| block.block_id)
                .collect::<Vec<_>>();
            inner.store.create_initial_document_with_provenance(
                &InitialProvenanceDocumentInput {
                    id: doc_id.clone(),
                    title,
                    markdown,
                    created_at: at,
                    updated_at: at,
                    actor: store_actor(actor),
                    update: ProvenanceUpdateInput {
                        expected_seq: update_seq,
                        payload: state.clone(),
                        actor_id: actor.id.clone(),
                        origin: actor.origin(),
                        session_id: actor.session_id.clone(),
                        created_at: at,
                    },
                    event,
                    changes: deltas_to_store(source_id, &reconciled.deltas)?,
                    spans,
                    lineage,
                    block_ids,
                    attributed_at: at,
                },
            )?;

            let prints = Inner::fingerprints(&doc);
            inner.docs.insert(doc_id.clone(), doc);
            inner.prints.insert(doc_id.clone(), prints);
            inner.lineages.insert(doc_id.clone(), reconciled.state);
            inner.pending.push((doc_id.clone(), state, actor.clone()));
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
        let context = default_context(actor, MutationContext::entered());
        self.replace_block_with_context(doc_id, block_id, markdown, version, actor, &context)
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
            let ((block, warnings), version) =
                inner.mutate(doc_id, actor, context, |doc| -> Result<_, WorkspaceError> {
                    let warnings = staleness(doc, version);
                    let block = doc.replace_block(block_id, first)?;
                    // Extra blocks in the payload follow the one replaced rather than
                    // being silently dropped.
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
        let context = default_context(actor, MutationContext::entered());
        self.insert_blocks_with_context(doc_id, after, markdown, version, actor, &context)
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
            let ((created, warnings), version) =
                inner.mutate(doc_id, actor, context, |doc| -> Result<_, WorkspaceError> {
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
        let context = default_context(actor, MutationContext::command());
        self.delete_block_with_context(doc_id, block_id, version, actor, &context)
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
            let (warnings, version) =
                inner.mutate(doc_id, actor, context, |doc| -> Result<_, WorkspaceError> {
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
        let context = default_context(actor, MutationContext::entered());
        self.replace_text_with_context(doc_id, block_id, edit, version, actor, &context)
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
        let context = default_context(actor, MutationContext::unknown());
        self.apply_peer_update_with_context(doc_id, update, actor, &context)
    }

    pub fn apply_peer_update_with_context(
        &self,
        doc_id: &str,
        update: &[u8],
        actor: &ActorRef,
        context: &MutationContext,
    ) -> Result<Option<String>, WorkspaceError> {
        self.with(|inner| {
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

            let version = inner.commit_candidate(doc_id, candidate, actor, context)?;
            Ok(Some(version))
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
        let context = default_context(actor, MutationContext::command());
        self.set_document_deleted_with_context(doc_id, deleted, actor, &context)
    }

    pub fn set_document_deleted_with_context(
        &self,
        doc_id: &str,
        deleted: bool,
        actor: &ActorRef,
        context: &MutationContext,
    ) -> Result<EditOutcome, WorkspaceError> {
        self.with(|inner| {
            let (_, version) =
                inner.mutate(doc_id, actor, context, |doc| -> Result<_, WorkspaceError> {
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

    /// Current source spans and grapheme counts for the visible wording.
    pub fn document_lineage(&self, doc_id: &str) -> Result<DocumentLineage, WorkspaceError> {
        self.with(|inner| {
            inner.doc(doc_id)?;
            let lineage = inner
                .lineages
                .get(doc_id)
                .expect("document hydration installs lineage");
            Ok(DocumentLineage {
                algorithm_version: LINEAGE_ALGORITHM_VERSION,
                alignment: "deterministic_inference",
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
    /// Run a mutation against a disposable replica, then install it only after
    /// the complete document and provenance transaction commits.
    fn mutate<T>(
        &mut self,
        doc_id: &str,
        actor: &ActorRef,
        context: &MutationContext,
        operation: impl FnOnce(&Document) -> Result<T, WorkspaceError>,
    ) -> Result<(T, String), WorkspaceError> {
        let current_state = self.doc(doc_id)?.encode_state();
        let candidate = Document::new();
        candidate.apply_update(&current_state).map_err(|error| {
            WorkspaceError::NotFound(format!("could not clone document state: {error}"))
        })?;
        let value = operation(&candidate)?;
        let version = self.commit_candidate(doc_id, candidate, actor, context)?;
        Ok((value, version))
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

            // Compatibility backfill and semantic-lineage hydration are one
            // logical cache installation. If either fails, do not leave a
            // document-only half-cache that skips verification on retry and
            // later panics when a mutation expects lineage to exist.
            let hydration = (|| {
                // Documents written before the block table existed have a
                // full log and no compatibility attribution. Replay pays that
                // off once.
                if !self.store.has_provenance(doc_id)? {
                    self.backfill(doc_id)?;
                }
                self.hydrate_lineage(doc_id)
            })();
            if let Err(error) = hydration {
                self.docs.remove(doc_id);
                self.prints.remove(doc_id);
                self.lineages.remove(doc_id);
                return Err(error);
            }
        }
        Ok(self.docs.get(doc_id).expect("just inserted"))
    }

    fn hydrate_lineage(&mut self, doc_id: &str) -> Result<(), WorkspaceError> {
        let tree = normalize(
            &self
                .docs
                .get(doc_id)
                .expect("document is loaded before lineage")
                .read(),
        );
        let snapshots = block_snapshots(self.docs.get(doc_id).expect("document is loaded"), &tree)?;
        let events = self.store.provenance_events(doc_id)?;
        if events.is_empty() {
            let lineage = self.seed_legacy_lineage(doc_id, snapshots)?;
            self.lineages.insert(doc_id.to_string(), lineage);
            return Ok(());
        }

        verify_stored_chain(&self.store, doc_id, &events)?;

        if let Some(lineage) = self.load_lineage_cache(doc_id, &snapshots, &events)? {
            self.lineages.insert(doc_id.to_string(), lineage);
            return Ok(());
        }

        let lineage = self.rebuild_lineage(doc_id, &events)?;
        self.lineages.insert(doc_id.to_string(), lineage);
        Ok(())
    }

    fn load_lineage_cache(
        &self,
        doc_id: &str,
        snapshots: &[thought_provenance::BlockSnapshot],
        events: &[thought_store::ProvenanceEventRow],
    ) -> Result<Option<LineageState>, WorkspaceError> {
        let Some(stored_state) = self.store.lineage_state(doc_id)? else {
            return Ok(None);
        };
        if stored_state.state != "ready"
            || stored_state.algorithm_version != i64::from(LINEAGE_ALGORITHM_VERSION)
        {
            return Ok(None);
        }
        let last_event = events.last().expect("events are nonempty");
        let last_update = self
            .store
            .updates_for_rebuild(doc_id)?
            .last()
            .map(|update| update.seq)
            .unwrap_or(0);
        if stored_state.through_event_id != last_event.event_id
            || stored_state.through_update_seq != last_update
            || last_event.after_hash.as_slice()
                != document_digest(
                    snapshots,
                    self.docs
                        .get(doc_id)
                        .expect("document is loaded before lineage")
                        .deleted_at(),
                )
        {
            return Ok(None);
        }

        let rows = self.store.lineage_spans(doc_id)?;
        let mut parts = lineage_from_store(doc_id, &rows, events)?;
        sort_spans(snapshots, &mut parts.spans);
        let lineage = match LineageState::from_parts(snapshots.to_vec(), parts.spans, parts.sources)
        {
            Ok(lineage) => lineage,
            Err(
                ReconcileError::UncoveredText(_)
                | ReconcileError::InvalidSpan(_)
                | ReconcileError::OverlappingSpans(_, _),
            ) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let digest = live_lineage_digest(&LineageHashInput {
            algorithm_version: LINEAGE_ALGORITHM_VERSION,
            document_id: doc_id,
            through_update_seq: as_u64(stored_state.through_update_seq, "update sequence")?,
            through_event_id: source_id(stored_state.through_event_id)?,
            spans: lineage.spans(),
        });
        if stored_state.lineage_digest.as_slice() != digest {
            return Ok(None);
        }
        Ok(Some(lineage))
    }

    fn seed_legacy_lineage(
        &self,
        doc_id: &str,
        snapshots: Vec<thought_provenance::BlockSnapshot>,
    ) -> Result<LineageState, WorkspaceError> {
        let context = MutationContext::legacy_seed();
        let event_id = self.store.next_provenance_event_id()?;
        let source_id = source_id(event_id)?;
        let source = context.source(source_id);
        let empty = LineageState::seed(vec![], source.clone())?;
        let reconciled = empty.reconcile(snapshots.clone(), source)?;
        let updates = self.store.updates_for_rebuild(doc_id)?;
        let update_seq = updates.last().map(|update| update.seq);
        let update_log_root = update_log_root_for_rows(doc_id, &updates)?;
        let at = now_ms();
        let event = event_input(EventBuild {
            event_id,
            update_seq,
            doc_id,
            actor: None,
            context: &context,
            action: context.action(),
            before_hash: document_digest(&[], None),
            after_hash: document_digest(
                &snapshots,
                self.docs
                    .get(doc_id)
                    .expect("document is loaded before lineage")
                    .deleted_at(),
            ),
            update_log_root,
            previous_hash: None,
            deltas: &reconciled.deltas,
            created_at: at,
            recorded_at: at,
        })?;
        let through_update_seq = update_seq.unwrap_or(0);
        self.store
            .record_provenance_without_update(&ProvenanceRecordInput {
                doc_id: doc_id.to_string(),
                event,
                changes: deltas_to_store(source_id, &reconciled.deltas)?,
                spans: spans_to_store(reconciled.state.spans())?,
                lineage: ready_lineage(
                    doc_id,
                    through_update_seq,
                    event_id,
                    reconciled.state.spans(),
                    at,
                )?,
                bind_to_latest_update: update_seq.is_some(),
            })?;
        Ok(reconciled.state)
    }

    fn rebuild_lineage(
        &self,
        doc_id: &str,
        events: &[thought_store::ProvenanceEventRow],
    ) -> Result<LineageState, WorkspaceError> {
        let updates = self.store.updates_for_rebuild(doc_id)?;
        let descriptors = lineage_from_store(doc_id, &[], events)?.sources;
        let replay = Document::new();
        let update_roots = update_log_roots(doc_id, &updates)?;
        let mut update_position = 0_usize;
        let mut lineage: Option<LineageState> = None;
        let mut previous_hash = None;
        let mut previous_document_hash = document_digest(&[], None);

        for event in events {
            if let Some(target) = event.update_seq {
                while update_position < updates.len() && updates[update_position].seq <= target {
                    replay
                        .apply_update(&updates[update_position].payload)
                        .map_err(|error| {
                            WorkspaceError::NotFound(format!(
                                "could not replay update {} for provenance: {error}",
                                updates[update_position].seq
                            ))
                        })?;
                    update_position += 1;
                }
                if update_position == 0 || updates[update_position - 1].seq != target {
                    return Err(WorkspaceError::NotFound(format!(
                        "provenance event {} refers to missing update {target}",
                        event.event_id
                    )));
                }
            }

            let tree = normalize(&replay.read());
            let after = block_snapshots(&replay, &tree)?;
            let source_id = source_id(event.event_id)?;
            let source = descriptors.get(&source_id).cloned().ok_or_else(|| {
                WorkspaceError::NotFound(format!(
                    "provenance event {} has no source descriptor",
                    event.event_id
                ))
            })?;
            let before_hash = previous_document_hash;
            let base = match lineage.take() {
                Some(state) => state,
                None => LineageState::seed(vec![], source.clone())?,
            };
            let reconciled = base.reconcile(after.clone(), source)?;
            let after_hash = document_digest(&after, replay.deleted_at());
            let update_log_root = event
                .update_seq
                .and_then(|seq| update_roots.get(&seq).copied())
                .or_else(|| {
                    update_position
                        .checked_sub(1)
                        .and_then(|position| update_roots.get(&updates[position].seq).copied())
                })
                .unwrap_or_else(|| empty_update_log_digest(doc_id));
            verify_rebuilt_event(
                &self.store,
                event,
                doc_id,
                RebuiltEventState {
                    before_hash,
                    after_hash,
                    update_log_root,
                    previous_hash,
                },
                &reconciled.deltas,
            )?;
            previous_hash = Some(digest_from_bytes(&event.event_hash, "event hash")?);
            previous_document_hash = after_hash;
            lineage = Some(reconciled.state);
        }

        if update_position != updates.len() {
            return Err(WorkspaceError::NotFound(format!(
                "document `{doc_id}` has updates with no provenance event"
            )));
        }
        let lineage = lineage.expect("events are nonempty");
        let current = self.docs.get(doc_id).expect("document is loaded");
        if replay.encode_state() != current.encode_state() {
            return Err(WorkspaceError::NotFound(format!(
                "provenance replay for `{doc_id}` does not match the document"
            )));
        }

        let through_update_seq = updates.last().map(|update| update.seq).unwrap_or(0);
        let through_event_id = events.last().expect("events are nonempty").event_id;
        self.store.rebuild_lineage_cache(&LineageRebuildInput {
            doc_id: doc_id.to_string(),
            spans: spans_to_store(lineage.spans())?,
            lineage: ready_lineage(
                doc_id,
                through_update_seq,
                through_event_id,
                lineage.spans(),
                now_ms(),
            )?,
            through_update_seq,
            through_event_id,
        })?;
        Ok(lineage)
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

    /// Persist a disposable candidate, then make it the in-memory authority.
    fn commit_candidate(
        &mut self,
        doc_id: &str,
        candidate: Document,
        actor: &ActorRef,
        context: &MutationContext,
    ) -> Result<String, WorkspaceError> {
        let current = self.docs.get(doc_id).expect("document is loaded");
        let before_vector = current.state_vector();
        let delta = candidate.diff_since(&before_vector);
        let before_tree = normalize(&current.read());
        let after_tree = normalize(&candidate.read());
        let before_snapshots = block_snapshots(current, &before_tree)?;
        let after_snapshots = block_snapshots(&candidate, &after_tree)?;
        let previous_lineage = self
            .lineages
            .get(doc_id)
            .expect("document hydration installs lineage")
            .clone();

        let event_id = self.store.next_provenance_event_id()?;
        let update_seq = self.store.next_update_seq()?;
        let source_id = source_id(event_id)?;
        let reconciled =
            previous_lineage.reconcile(after_snapshots.clone(), context.source(source_id))?;
        let prior_event = self.store.latest_provenance_event(doc_id)?;
        let previous_hash = prior_event
            .as_ref()
            .map(|event| digest_from_bytes(&event.event_hash, "event hash"))
            .transpose()?;
        let previous_update_root = prior_event
            .as_ref()
            .map(|event| digest_from_bytes(&event.update_log_root, "update log root"))
            .transpose()?;
        let before_hash = document_digest(&before_snapshots, current.deleted_at());
        if let Some(last) = prior_event.as_ref()
            && last.after_hash.as_slice() != before_hash
        {
            return Err(WorkspaceError::NotFound(format!(
                "document `{doc_id}` does not match its last provenance event"
            )));
        }
        let after_hash = document_digest(&after_snapshots, candidate.deleted_at());
        let action = if current.deleted_at() == candidate.deleted_at() {
            context.action()
        } else if candidate.deleted_at().is_some() {
            EventAction::Trash
        } else {
            EventAction::Restore
        };
        let at = now_ms();
        let update_log_root =
            extend_update_log_root(previous_update_root, doc_id, update_seq, &delta, actor, at)?;
        let event = event_input(EventBuild {
            event_id,
            update_seq: Some(update_seq),
            doc_id,
            actor: Some(actor),
            context,
            action,
            before_hash,
            after_hash,
            update_log_root,
            previous_hash,
            deltas: &reconciled.deltas,
            created_at: at,
            recorded_at: at,
        })?;

        let current_prints = Self::fingerprints(&candidate);
        let previous_prints = self.prints.get(doc_id);
        let block_touches = current_prints
            .iter()
            .filter(|(block_id, print)| {
                previous_prints.and_then(|prints| prints.get(*block_id)) != Some(*print)
            })
            .map(|(block_id, _)| BlockTouchInput {
                block_id: block_id.clone(),
                actor_id: actor.id.clone(),
                session_id: actor.session_id.clone(),
                at,
            })
            .collect::<Vec<_>>();
        let current_block_ids = candidate
            .blocks()
            .into_iter()
            .map(|block| block.block_id)
            .collect::<Vec<_>>();
        let (markdown, _) = to_markdown_with_spans(&after_tree);
        let state = candidate.encode_state();
        let state_vector = candidate.state_vector();
        let persisted = self
            .store
            .commit_update_with_provenance(&ProvenanceCommitInput {
                doc_id: doc_id.to_string(),
                title: derive_title(&after_tree),
                markdown,
                deleted_at: candidate.deleted_at(),
                updated_at: at,
                actor: store_actor(actor),
                update: ProvenanceUpdateInput {
                    expected_seq: update_seq,
                    payload: delta.clone(),
                    actor_id: actor.id.clone(),
                    origin: actor.origin(),
                    session_id: actor.session_id.clone(),
                    created_at: at,
                },
                event,
                changes: deltas_to_store(source_id, &reconciled.deltas)?,
                spans: spans_to_store(reconciled.state.spans())?,
                lineage: ready_lineage(doc_id, update_seq, event_id, reconciled.state.spans(), at)?,
                block_touches,
                current_block_ids,
            })?;

        self.docs.insert(doc_id.to_string(), candidate);
        self.prints.insert(doc_id.to_string(), current_prints);
        self.lineages.insert(doc_id.to_string(), reconciled.state);
        self.pending
            .push((doc_id.to_string(), delta, actor.clone()));

        if self
            .store
            .updates_since_snapshot(doc_id)
            .is_ok_and(|count| count >= SNAPSHOT_EVERY)
            && let Some(seq) = persisted.update_seq
        {
            // Snapshots are a discardable performance cache. An authoritative
            // commit must not be reported as failed if this follow-up write is
            // unavailable.
            let _ = self
                .store
                .write_snapshot(doc_id, seq, &state, &state_vector);
        }
        Ok(encode_version(&state_vector))
    }
}

struct EventBuild<'a> {
    event_id: i64,
    update_seq: Option<i64>,
    doc_id: &'a str,
    actor: Option<&'a ActorRef>,
    context: &'a MutationContext,
    action: EventAction,
    before_hash: [u8; 32],
    after_hash: [u8; 32],
    update_log_root: [u8; 32],
    previous_hash: Option<[u8; 32]>,
    deltas: &'a [thought_provenance::DeltaSegment],
    created_at: i64,
    recorded_at: i64,
}

fn event_input(build: EventBuild<'_>) -> Result<ProvenanceEventInput, WorkspaceError> {
    let source_id = source_id(build.event_id)?;
    let actor_id = build.actor.map(|actor| actor.id.as_str());
    let actor_label = build
        .actor
        .map(|actor| actor.display_name.as_str())
        .unwrap_or("Unknown");
    let session_id = build.actor.and_then(|actor| actor.session_id.as_deref());
    let reported_model = build
        .context
        .reported_model()
        .or_else(|| build.actor.and_then(|actor| actor.model.as_deref()));
    let hash = event_chain_digest(&EventHashInput {
        chain_version: CURRENT_EVENT_CHAIN_VERSION,
        event_id: source_id,
        document_id: build.doc_id,
        update_seq: build
            .update_seq
            .map(|seq| as_u64(seq, "update sequence"))
            .transpose()?,
        action: build.action,
        ingress: build.context.ingress(),
        assurance: build.context.assurance(),
        source_label: build.context.source_label(),
        actor: ActorEventMetadata {
            actor_id,
            actor_label,
            provider: build.context.provider(),
            requested_model: build.context.requested_model(),
            reported_model,
            connection_id: build.context.connection_id(),
            session_id,
        },
        references: EventReferences {
            evidence_ref: build.context.evidence_ref(),
            suggestion_id: build.context.suggestion_id(),
            client_event_id: build.context.client_event_id(),
        },
        created_at_ms: build.created_at,
        recorded_at_ms: build.recorded_at,
        before_document_hash: build.before_hash,
        after_document_hash: build.after_hash,
        update_log_root: build.update_log_root,
        previous_event_hash: build.previous_hash,
        deltas: build.deltas,
    });

    Ok(ProvenanceEventInput {
        event_id: build.event_id,
        actor_id: actor_id.map(str::to_string),
        action: action_name(build.action).to_string(),
        ingress: ingress_name(build.context.ingress()).to_string(),
        assurance: assurance_name(build.context.assurance()).to_string(),
        connection_id: build.context.connection_id().map(str::to_string),
        session_id: session_id.map(str::to_string),
        actor_label: actor_label.to_string(),
        source_label: build.context.source_label().to_string(),
        provider: build.context.provider().map(str::to_string),
        requested_model: build.context.requested_model().map(str::to_string),
        reported_model: reported_model.map(str::to_string),
        evidence_ref: build.context.evidence_ref().map(str::to_string),
        suggestion_id: build.context.suggestion_id().map(str::to_string),
        client_event_id: build.context.client_event_id().map(str::to_string),
        chain_version: i64::from(CURRENT_EVENT_CHAIN_VERSION),
        before_hash: build.before_hash.to_vec(),
        after_hash: build.after_hash.to_vec(),
        update_log_root: build.update_log_root.to_vec(),
        previous_event_hash: build.previous_hash.map(|hash| hash.to_vec()),
        event_hash: hash.to_vec(),
        created_at: build.created_at,
        recorded_at: build.recorded_at,
    })
}

fn ready_lineage(
    doc_id: &str,
    update_seq: i64,
    event_id: i64,
    spans: &[thought_provenance::LiveLineageSpan],
    rebuilt_at: i64,
) -> Result<ReadyLineageInput, WorkspaceError> {
    let digest = live_lineage_digest(&LineageHashInput {
        algorithm_version: LINEAGE_ALGORITHM_VERSION,
        document_id: doc_id,
        through_update_seq: as_u64(update_seq, "update sequence")?,
        through_event_id: source_id(event_id)?,
        spans,
    });
    Ok(ReadyLineageInput {
        algorithm_version: i64::from(LINEAGE_ALGORITHM_VERSION),
        lineage_digest: digest.to_vec(),
        rebuilt_at,
    })
}

fn verify_stored_chain(
    store: &Store,
    doc_id: &str,
    events: &[thought_store::ProvenanceEventRow],
) -> Result<(), WorkspaceError> {
    let updates = store.updates_for_rebuild(doc_id)?;
    let update_roots = update_log_roots(doc_id, &updates)?;
    let mut previous_event_hash = None;
    let mut previous_document_hash = document_digest(&[], None);
    let mut previous_update_root = empty_update_log_digest(doc_id);
    let mut previous_update_seq = None;
    for event in events {
        let before_hash = digest_from_bytes(&event.before_hash, "before document hash")?;
        let after_hash = digest_from_bytes(&event.after_hash, "after document hash")?;
        if before_hash != previous_document_hash
            || event.previous_event_hash.as_deref()
                != previous_event_hash
                    .as_ref()
                    .map(|hash: &[u8; 32]| hash.as_slice())
        {
            return Err(WorkspaceError::NotFound(format!(
                "provenance event {} breaks the document hash chain",
                event.event_id
            )));
        }
        let expected_update_root = match event.update_seq {
            Some(seq) => {
                if previous_update_seq.is_some_and(|previous| seq <= previous) {
                    return Err(WorkspaceError::NotFound(format!(
                        "provenance event {} has a non-increasing update sequence",
                        event.event_id
                    )));
                }
                previous_update_seq = Some(seq);
                update_roots.get(&seq).copied().ok_or_else(|| {
                    WorkspaceError::NotFound(format!(
                        "provenance event {} refers to missing update {seq}",
                        event.event_id
                    ))
                })?
            }
            None => previous_update_root,
        };
        if event.update_log_root.as_slice() != expected_update_root {
            return Err(WorkspaceError::NotFound(format!(
                "provenance event {} has an invalid update log root",
                event.event_id
            )));
        }
        let event_id = source_id(event.event_id)?;
        let deltas = deltas_from_store(event_id, &store.provenance_changes(event.event_id)?)?;
        let computed = event_digest_from_row(
            event,
            doc_id,
            before_hash,
            after_hash,
            expected_update_root,
            previous_event_hash,
            &deltas,
        )?;
        if event.event_hash.as_slice() != computed {
            return Err(WorkspaceError::NotFound(format!(
                "provenance event {} has an invalid event hash",
                event.event_id
            )));
        }
        previous_event_hash = Some(computed);
        previous_document_hash = after_hash;
        previous_update_root = expected_update_root;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct RebuiltEventState {
    before_hash: [u8; 32],
    after_hash: [u8; 32],
    update_log_root: [u8; 32],
    previous_hash: Option<[u8; 32]>,
}

fn verify_rebuilt_event(
    store: &Store,
    event: &thought_store::ProvenanceEventRow,
    doc_id: &str,
    state: RebuiltEventState,
    deltas: &[thought_provenance::DeltaSegment],
) -> Result<(), WorkspaceError> {
    if event.before_hash.as_slice() != state.before_hash
        || event.after_hash.as_slice() != state.after_hash
        || event.update_log_root.as_slice() != state.update_log_root
        || event.previous_event_hash.as_deref()
            != state.previous_hash.as_ref().map(|hash| hash.as_slice())
    {
        return Err(WorkspaceError::NotFound(format!(
            "provenance event {} has an invalid document hash chain",
            event.event_id
        )));
    }
    let event_id = source_id(event.event_id)?;
    let stored_deltas = deltas_from_store(event_id, &store.provenance_changes(event.event_id)?)?;
    if stored_deltas != deltas {
        return Err(WorkspaceError::NotFound(format!(
            "provenance event {} does not match its semantic changes",
            event.event_id
        )));
    }
    let computed = event_digest_from_row(
        event,
        doc_id,
        state.before_hash,
        state.after_hash,
        state.update_log_root,
        state.previous_hash,
        deltas,
    )?;
    if event.event_hash.as_slice() != computed {
        return Err(WorkspaceError::NotFound(format!(
            "provenance event {} has an invalid event hash",
            event.event_id
        )));
    }
    Ok(())
}

fn event_digest_from_row(
    event: &thought_store::ProvenanceEventRow,
    doc_id: &str,
    before_hash: [u8; 32],
    after_hash: [u8; 32],
    update_log_root: [u8; 32],
    previous_hash: Option<[u8; 32]>,
    deltas: &[thought_provenance::DeltaSegment],
) -> Result<[u8; 32], WorkspaceError> {
    let event_id = source_id(event.event_id)?;
    let chain_version = u32::try_from(event.chain_version).map_err(|_| {
        WorkspaceError::NotFound(format!(
            "provenance event {} has an invalid chain version",
            event.event_id
        ))
    })?;
    if chain_version != CURRENT_EVENT_CHAIN_VERSION {
        return Err(WorkspaceError::NotFound(format!(
            "provenance event {} uses unsupported chain version {}; this build supports {}",
            event.event_id, chain_version, CURRENT_EVENT_CHAIN_VERSION
        )));
    }
    Ok(event_chain_digest(&EventHashInput {
        chain_version,
        event_id,
        document_id: doc_id,
        update_seq: event
            .update_seq
            .map(|seq| as_u64(seq, "update sequence"))
            .transpose()?,
        action: parse_action(&event.action)?,
        ingress: parse_ingress(&event.ingress)?,
        assurance: parse_assurance(&event.assurance)?,
        source_label: &event.source_label,
        actor: ActorEventMetadata {
            actor_id: event.actor_id.as_deref(),
            actor_label: &event.actor_label,
            provider: event.provider.as_deref(),
            requested_model: event.requested_model.as_deref(),
            reported_model: event.reported_model.as_deref(),
            connection_id: event.connection_id.as_deref(),
            session_id: event.session_id.as_deref(),
        },
        references: EventReferences {
            evidence_ref: event.evidence_ref.as_deref(),
            suggestion_id: event.suggestion_id.as_deref(),
            client_event_id: event.client_event_id.as_deref(),
        },
        created_at_ms: event.created_at,
        recorded_at_ms: event.recorded_at,
        before_document_hash: before_hash,
        after_document_hash: after_hash,
        update_log_root,
        previous_event_hash: previous_hash,
        deltas,
    }))
}

fn extend_update_log_root(
    previous: Option<[u8; 32]>,
    doc_id: &str,
    seq: i64,
    payload: &[u8],
    actor: &ActorRef,
    created_at: i64,
) -> Result<[u8; 32], WorkspaceError> {
    Ok(update_log_digest(
        previous,
        &UpdateLogEntry {
            document_id: doc_id,
            seq: as_u64(seq, "update sequence")?,
            payload,
            actor_id: &actor.id,
            origin: actor.origin().as_str(),
            session_id: actor.session_id.as_deref(),
            created_at_ms: created_at,
        },
    ))
}

fn update_log_roots(
    doc_id: &str,
    updates: &[thought_store::EvidenceUpdate],
) -> Result<HashMap<i64, [u8; 32]>, WorkspaceError> {
    let mut roots = HashMap::with_capacity(updates.len());
    let mut previous = None;
    let mut previous_seq = None;
    for update in updates {
        if previous_seq.is_some_and(|seq| update.seq <= seq) {
            return Err(WorkspaceError::NotFound(format!(
                "document `{doc_id}` has a non-increasing update sequence"
            )));
        }
        let root = update_log_digest(
            previous,
            &UpdateLogEntry {
                document_id: doc_id,
                seq: as_u64(update.seq, "update sequence")?,
                payload: &update.payload,
                actor_id: &update.actor_id,
                origin: &update.origin,
                session_id: update.session_id.as_deref(),
                created_at_ms: update.created_at,
            },
        );
        roots.insert(update.seq, root);
        previous = Some(root);
        previous_seq = Some(update.seq);
    }
    Ok(roots)
}

fn update_log_root_for_rows(
    doc_id: &str,
    updates: &[thought_store::EvidenceUpdate],
) -> Result<[u8; 32], WorkspaceError> {
    let roots = update_log_roots(doc_id, updates)?;
    Ok(updates
        .last()
        .and_then(|update| roots.get(&update.seq).copied())
        .unwrap_or_else(|| empty_update_log_digest(doc_id)))
}

fn default_context(actor: &ActorRef, local: MutationContext) -> MutationContext {
    if actor.kind == "agent" {
        MutationContext::mcp_reported(actor.display_name.clone(), None, None, actor.model.clone())
    } else {
        local
    }
}

fn store_actor(actor: &ActorRef) -> Actor {
    Actor {
        id: actor.id.clone(),
        kind: actor.kind.clone(),
        display_name: actor.display_name.clone(),
        model: actor.model.clone(),
        color: color_for(&actor.id),
    }
}

fn source_id(value: i64) -> Result<SourceId, WorkspaceError> {
    Ok(SourceId(as_u64(value, "provenance event id")?))
}

fn as_u64(value: i64, field: &str) -> Result<u64, WorkspaceError> {
    u64::try_from(value).map_err(|_| {
        WorkspaceError::NotFound(format!("{field} must be nonnegative, found {value}"))
    })
}

fn digest_from_bytes(bytes: &[u8], field: &str) -> Result<[u8; 32], WorkspaceError> {
    bytes.try_into().map_err(|_| {
        WorkspaceError::NotFound(format!(
            "{field} must contain exactly 32 bytes, found {}",
            bytes.len()
        ))
    })
}

fn parse_action(value: &str) -> Result<crate::provenance_hash::EventAction, WorkspaceError> {
    use crate::provenance_hash::EventAction;
    match value {
        "edit" => Ok(EventAction::Edit),
        "trash" => Ok(EventAction::Trash),
        "restore" => Ok(EventAction::Restore),
        "legacy_seed" => Ok(EventAction::LegacySeed),
        "suggestion" => Ok(EventAction::Suggestion),
        "accept" => Ok(EventAction::Accept),
        "reject" => Ok(EventAction::Reject),
        other => Err(WorkspaceError::NotFound(format!(
            "unknown persisted provenance action `{other}`"
        ))),
    }
}

fn parse_ingress(value: &str) -> Result<Ingress, WorkspaceError> {
    match value {
        "entered" => Ok(Ingress::Entered),
        "command" => Ok(Ingress::Command),
        "pasted" => Ok(Ingress::Pasted),
        "imported" => Ok(Ingress::Imported),
        "mcp" => Ok(Ingress::Mcp),
        "api" => Ok(Ingress::Api),
        "suggestion" => Ok(Ingress::Suggestion),
        "unknown" => Ok(Ingress::Unknown),
        "legacy_unknown" => Ok(Ingress::LegacyUnknown),
        other => Err(WorkspaceError::NotFound(format!(
            "unknown persisted provenance ingress `{other}`"
        ))),
    }
}

fn parse_assurance(value: &str) -> Result<Assurance, WorkspaceError> {
    match value {
        "observed" => Ok(Assurance::Observed),
        "reported" => Ok(Assurance::Reported),
        "verified" => Ok(Assurance::Verified),
        "unknown" => Ok(Assurance::Unknown),
        other => Err(WorkspaceError::NotFound(format!(
            "unknown persisted provenance assurance `{other}`"
        ))),
    }
}

fn sort_spans(
    blocks: &[thought_provenance::BlockSnapshot],
    spans: &mut [thought_provenance::LiveLineageSpan],
) {
    let block_order = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.block_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    spans.sort_by(|left, right| {
        block_order
            .get(left.location.block_id.as_str())
            .cmp(&block_order.get(right.location.block_id.as_str()))
            .then_with(|| left.location.path.cmp(&right.location.path))
            .then_with(|| left.location.from_utf16.cmp(&right.location.from_utf16))
    });
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
