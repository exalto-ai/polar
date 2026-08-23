use base64::Engine;
use polar_core::{BlockError, Document, Position};
use polar_markdown::{from_markdown, to_markdown_with_spans};
use polar_schema::{Node, Schema, normalize};
use polar_store::{Actor, Origin, Store};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

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

pub struct Workspace {
    store: Store,
    docs: Mutex<HashMap<String, Document>>,
}

impl Workspace {
    pub fn open(path: impl AsRef<Path>) -> Result<Workspace, WorkspaceError> {
        Ok(Workspace {
            store: Store::open(path)?,
            docs: Mutex::new(HashMap::new()),
        })
    }

    pub fn open_in_memory() -> Result<Workspace, WorkspaceError> {
        Ok(Workspace {
            store: Store::open_in_memory()?,
            docs: Mutex::new(HashMap::new()),
        })
    }

    fn with_doc<T>(
        &self,
        doc_id: &str,
        f: impl FnOnce(&Document) -> T,
    ) -> Result<T, WorkspaceError> {
        let mut docs = self.docs.lock().expect("workspace mutex poisoned");
        if !docs.contains_key(doc_id) {
            // Lazy load: documents are hydrated on first touch, not at boot.
            let restored = self.store.restore(doc_id)?;
            if restored.snapshot.is_none() && restored.updates.is_empty() {
                return Err(WorkspaceError::NoSuchDocument(doc_id.to_string()));
            }
            let doc = Document::new();
            if let Some(state) = &restored.snapshot {
                let _ = doc.apply_update(state);
            }
            for update in &restored.updates {
                let _ = doc.apply_update(update);
            }
            docs.insert(doc_id.to_string(), doc);
        }
        Ok(f(docs.get(doc_id).expect("just inserted")))
    }

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

    /// Persist everything written since `before`, then keep the derived state
    /// (search index, denormalized title) in step.
    fn commit(
        &self,
        doc_id: &str,
        doc: &Document,
        before: &[u8],
        actor: &ActorRef,
    ) -> Result<String, WorkspaceError> {
        let delta = doc.diff_since(before);
        let seq = self.store.append_update(
            doc_id,
            &delta,
            &actor.id,
            actor.origin(),
            actor.session_id.as_deref(),
        )?;

        let tree = normalize(&doc.read());
        let (markdown, _) = to_markdown_with_spans(&tree);
        // Reindexed on every mutation, not on snapshot as the ADR first said.
        // Serializing a document and writing two rows is cheap; agents reading
        // a stale index is not, and search is how they avoid reading every
        // document.
        self.store
            .reindex(doc_id, &derive_title(&tree), &markdown)?;

        if self.store.updates_since_snapshot(doc_id)? >= SNAPSHOT_EVERY {
            self.store
                .write_snapshot(doc_id, seq, &doc.encode_state(), &doc.state_vector())?;
        }
        Ok(encode_version(&doc.state_vector()))
    }

    pub fn create_document(
        &self,
        title: &str,
        actor: &ActorRef,
    ) -> Result<DocumentView, WorkspaceError> {
        self.register(actor)?;
        let doc_id = uuid::Uuid::now_v7().to_string();
        self.store.create_document(&doc_id, title)?;

        let doc = Document::new();
        let seed = normalize(&Node::element(
            "doc",
            vec![Node::element("paragraph", vec![])],
        ));
        doc.set_document(&seed);

        self.store.append_update(
            &doc_id,
            &doc.encode_state(),
            &actor.id,
            actor.origin(),
            actor.session_id.as_deref(),
        )?;
        self.store.reindex(&doc_id, title, "")?;
        self.docs
            .lock()
            .expect("workspace mutex poisoned")
            .insert(doc_id.clone(), doc);

        self.read_document(&doc_id)
    }

    pub fn read_document(&self, doc_id: &str) -> Result<DocumentView, WorkspaceError> {
        let (tree, refs, version) = self.with_doc(doc_id, |doc| {
            (
                normalize(&doc.read()),
                doc.blocks(),
                encode_version(&doc.state_vector()),
            )
        })?;

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
    }

    pub fn list_documents(&self, limit: usize) -> Result<Vec<DocumentSummary>, WorkspaceError> {
        Ok(self
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
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, WorkspaceError> {
        let titles: HashMap<String, String> = self
            .store
            .list_documents()?
            .into_iter()
            .map(|d| (d.id, d.title))
            .collect();
        Ok(self
            .store
            .search(query, limit)?
            .into_iter()
            .map(|(doc_id, snippet)| SearchHit {
                title: titles.get(&doc_id).cloned().unwrap_or_default(),
                doc_id,
                snippet,
            })
            .collect())
    }

    /// A stale `version` **warns and proceeds**.
    ///
    /// The CRDT merges correctly regardless, so a stale read is not a conflict —
    /// it is a semantic risk that the agent reasoned about text which has since
    /// moved. Failing the call would punish the agent for something that is not
    /// an error and push callers toward re-reading the whole document, which is
    /// exactly the whole-document traffic AD-5 exists to prevent.
    fn staleness(&self, doc: &Document, version: Option<&str>) -> Vec<String> {
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

    fn parse_blocks(&self, markdown: &str) -> Result<Vec<Node>, WorkspaceError> {
        let parsed = normalize(&from_markdown(markdown));
        if let Err(errs) = Schema::v0().validate(&parsed) {
            return Err(WorkspaceError::InvalidMarkdown(
                errs.iter().map(ToString::to_string).collect(),
            ));
        }
        Ok(parsed.content)
    }

    pub fn replace_block(
        &self,
        doc_id: &str,
        block_id: &str,
        markdown: &str,
        version: Option<&str>,
        actor: &ActorRef,
    ) -> Result<EditOutcome, WorkspaceError> {
        self.register(actor)?;
        let nodes = self.parse_blocks(markdown)?;
        let Some(first) = nodes.first() else {
            return Err(WorkspaceError::InvalidMarkdown(vec![
                "markdown produced no blocks".into(),
            ]));
        };

        let (before, warnings, result) = self.with_doc(doc_id, |doc| {
            let before = doc.state_vector();
            let warnings = self.staleness(doc, version);
            let result = doc.replace_block(block_id, first);
            // Extra blocks in the payload follow the one being replaced, rather
            // than being silently dropped.
            if let Ok(ref block) = result
                && nodes.len() > 1
            {
                let _ = doc.insert_blocks(&Position::After(block.block_id.clone()), &nodes[1..]);
            }
            (before, warnings, result)
        })?;

        let block = result?;
        let version = self.with_doc(doc_id, |doc| self.commit(doc_id, doc, &before, actor))??;
        Ok(EditOutcome {
            doc_id: doc_id.into(),
            block_id: Some(block.block_id),
            version,
            warnings,
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
        self.register(actor)?;
        let nodes = self.parse_blocks(markdown)?;

        let (before, warnings, result) = self.with_doc(doc_id, |doc| {
            let before = doc.state_vector();
            let warnings = self.staleness(doc, version);
            (before, warnings, doc.insert_blocks(after, &nodes))
        })?;

        let created = result?;
        let version = self.with_doc(doc_id, |doc| self.commit(doc_id, doc, &before, actor))??;
        Ok(EditOutcome {
            doc_id: doc_id.into(),
            block_id: created.first().map(|b| b.block_id.clone()),
            version,
            warnings,
        })
    }

    pub fn delete_block(
        &self,
        doc_id: &str,
        block_id: &str,
        version: Option<&str>,
        actor: &ActorRef,
    ) -> Result<EditOutcome, WorkspaceError> {
        self.register(actor)?;
        let (before, warnings, result) = self.with_doc(doc_id, |doc| {
            let before = doc.state_vector();
            let warnings = self.staleness(doc, version);
            (before, warnings, doc.delete_block(block_id))
        })?;
        result?;
        let version = self.with_doc(doc_id, |doc| self.commit(doc_id, doc, &before, actor))??;
        Ok(EditOutcome {
            doc_id: doc_id.into(),
            block_id: None,
            version,
            warnings,
        })
    }

    /// Attribution for the whole log, for the activity feed and per-run revert.
    pub fn attribution(
        &self,
        doc_id: &str,
    ) -> Result<Vec<(String, Option<String>)>, WorkspaceError> {
        Ok(self
            .store
            .log(doc_id)?
            .into_iter()
            .map(|u| (u.actor_id, u.session_id))
            .collect())
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
