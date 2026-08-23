use base64::Engine;
use polar_core::{BlockError, Document, Position};
use polar_markdown::{from_markdown, to_markdown_with_spans};
use polar_schema::{Node, Schema, normalize};
use polar_store::{Actor, Origin, Store};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

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
    Storage(polar_store::SqlError),
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

impl From<polar_store::SqlError> for WorkspaceError {
    fn from(e: polar_store::SqlError) -> Self {
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
        let doc_id = self.with(|inner| -> Result<String, WorkspaceError> {
            inner.register(actor)?;
            let doc_id = uuid::Uuid::now_v7().to_string();
            inner.store.create_document(&doc_id, title)?;

            let doc = Document::new();
            doc.set_document(&normalize(&Node::element(
                "doc",
                vec![Node::element("paragraph", vec![])],
            )));
            inner.store.append_update(
                &doc_id,
                &doc.encode_state(),
                &actor.id,
                actor.origin(),
                actor.session_id.as_deref(),
            )?;
            inner.store.reindex(&doc_id, title, "")?;
            inner.docs.insert(doc_id.clone(), doc);
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

    pub fn list_documents(&self, limit: usize) -> Result<Vec<DocumentSummary>, WorkspaceError> {
        self.with(|inner| {
            Ok(inner
                .store
                .list_documents()?
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
                .list_documents()?
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
            let doc = inner.doc(doc_id)?;
            let before = doc.state_vector();
            doc.apply_update(update)
                .map_err(|e| WorkspaceError::NotFound(format!("bad update: {e}")))?;
            if doc.state_vector() == before {
                return Ok(None);
            }
            Ok(Some(inner.commit(doc_id, &before, actor)?))
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
        }
        Ok(self.docs.get(doc_id).expect("just inserted"))
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
