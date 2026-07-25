//! The lens core: a read-time view over `refs/comments/*` projected into
//! whatever buffer the client has open, plus the compose flow that writes
//! new comments back through the shared library.
//!
//! Every response is derived per request from an anchor projection onto the
//! working tree — never cached across a comment mutation (`lens.lenses`) —
//! and every listing, projection, and write is the exact `gix-comment`
//! library call a CLI porcelain would make (`lens.parity`), so a comment is
//! one entity across the editor and any other frontend.

use gix::ObjectId;
use gix_anchor::{Binding, capture_worktree, project_worktree};
use gix_comment::Comments;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeLens, CreateFile, CreateFileOptions,
    Diagnostic, DocumentChangeOperation, DocumentChanges, Hover, HoverContents, OneOf,
    OptionalVersionedTextDocumentIdentifier, Position, Range, ResourceOp, TextDocumentEdit,
    TextEdit, Url, WorkspaceEdit,
};
use serde_json::{Value, json};

use crate::compose::{self, Target};
use crate::document::{self, Documents};
use crate::error::{Error, Result};
use crate::render;

/// What an `executeCommand` or a `didSave` produced, in protocol-neutral
/// terms the server layer turns into LSP messages: an optional command
/// result value, an optional workspace edit to apply, and whether the open
/// documents' diagnostics should be republished (a comment mutation
/// invalidates every derived view, `lens.lenses`).
#[derive(Debug, Default)]
pub struct Outcome {
    /// The `workspace/executeCommand` result value (the thread markup, for
    /// View); `None` for a command whose effect is a side effect only.
    pub response: Option<Value>,
    /// A workspace edit the client should apply (`workspace/applyEdit`) —
    /// creating and filling in a compose or reply template (`lens.compose`).
    pub edit: Option<WorkspaceEdit>,
    /// Whether every open document's diagnostics should be recomputed and
    /// republished, because a comment just changed.
    pub refresh: bool,
}

/// One comment landed onto an open document at a specific range
/// (`lens.lenses`, `lens.working-tree`): the projected `range`, whether the
/// projection was [`gix_anchor::Projection::Outdated`], and how many replies
/// the thread it roots has.
struct Landed {
    id: ObjectId,
    comment: gix_comment::Comment,
    range: Range,
    outdated: bool,
    replies: usize,
}

/// The editor surface over one repository's comments (`lens.serve`). Owns
/// the repository and the client's open buffers; derives every response
/// fresh, caching nothing.
pub struct Lens {
    repo: gix::Repository,
    documents: Documents,
}

impl Lens {
    /// Wire a lens over an already-opened repository — the one constructor
    /// a composition root calls (`git comment lsp`).
    #[must_use]
    pub fn new(repo: gix::Repository) -> Self {
        Self {
            repo,
            documents: Documents::default(),
        }
    }

    /// Record a document the client opened, with its full text
    /// (`textDocument/didOpen`) — projection targets this buffer afterward
    /// (`lens.working-tree`).
    pub fn did_open(&mut self, uri: Url, text: String) {
        self.documents.set(uri, text);
    }

    /// Replace an open document's text on a full-sync change
    /// (`textDocument/didChange`), so ranges re-project against the unsaved
    /// edit (`lens.working-tree`).
    pub fn did_change(&mut self, uri: Url, text: String) {
        self.documents.set(uri, text);
    }

    /// Forget a closed document (`textDocument/didClose`); projection falls
    /// back to on-disk bytes.
    pub fn did_close(&mut self, uri: &Url) {
        self.documents.remove(uri);
    }

    /// Every open document's URI — the server republishes diagnostics for
    /// these after a mutation ([`Outcome::refresh`]).
    #[must_use]
    pub fn open_documents(&self) -> Vec<Url> {
        self.documents.open_uris()
    }

    /// The open, position-bound root comments anchored to `uri`'s document
    /// (`model.comment-state`), each projected onto its live buffer when
    /// open, on-disk bytes otherwise — the one derivation every read
    /// response is built from, recomputed here on every call and never
    /// cached (`lens.lenses`, `lens.working-tree`, `lens.parity`).
    ///
    /// Only a position-bound comment (`Binding::Position`) whose anchor
    /// names `uri`'s own path can land on a line range; a comment on a
    /// whole commit or tree has none, so it never appears here.
    fn document_comments(&self, uri: &Url) -> Result<Vec<Landed>> {
        let Some(workdir) = self.repo.workdir() else {
            return Ok(Vec::new());
        };
        let Some(rel) = document::relative_path(workdir, uri) else {
            return Ok(Vec::new());
        };
        let buffer = self.documents.text(uri).map(str::as_bytes);
        let comments = Comments::open(&self.repo);
        let mut out = Vec::new();
        for comment in comments.list_roots(None, false)? {
            let Binding::Position(anchor) = &comment.binding else {
                continue;
            };
            if anchor.path != rel {
                continue;
            }
            let projection = project_worktree(&self.repo, anchor, buffer)?;
            let Some((range, outdated)) = render::landed_range(&projection, anchor) else {
                continue;
            };
            let replies = comments.thread(comment.id)?.replies.len();
            out.push(Landed {
                id: comment.id,
                comment,
                range,
                outdated,
                replies,
            });
        }
        Ok(out)
    }

    /// The code lenses for `uri` (`lens.lenses`): a View/Reply/Resolve lens
    /// set per open comment projecting onto the document, omitting a
    /// comment whose anchor no longer lands there.
    ///
    /// # Errors
    ///
    /// Propagates a repository, comment-store, or projection failure.
    pub fn code_lenses(&self, uri: &Url) -> Result<Vec<CodeLens>> {
        let mut out = Vec::new();
        for landed in self.document_comments(uri)? {
            out.extend(render::code_lenses(
                landed.id,
                &landed.comment,
                landed.range,
                landed.outdated,
                landed.replies,
            ));
        }
        Ok(out)
    }

    /// The hint-severity diagnostics for `uri` (`lens.diagnostics`): the
    /// same projected comments as the lenses, one hint each, for clients
    /// that do not render lenses. Never a warning or error.
    ///
    /// # Errors
    ///
    /// Propagates a repository, comment-store, or projection failure.
    pub fn diagnostics(&self, uri: &Url) -> Result<Vec<Diagnostic>> {
        let mut out = Vec::new();
        for landed in self.document_comments(uri)? {
            out.push(render::diagnostic(
                landed.id,
                &landed.comment,
                landed.range,
                landed.outdated,
            ));
        }
        Ok(out)
    }

    /// The hover for a position in `uri` (`lens.hover`): if it falls on a
    /// projected comment's range, the whole thread rendered as Markdown.
    ///
    /// # Errors
    ///
    /// Propagates a repository, comment-store, or projection failure.
    pub fn hover(&self, uri: &Url, position: Position) -> Result<Option<Hover>> {
        for landed in self.document_comments(uri)? {
            if !position_in(position, landed.range) {
                continue;
            }
            let comments = Comments::open(&self.repo);
            let thread = comments.thread(landed.id)?;
            return Ok(Some(Hover {
                contents: HoverContents::Markup(render::hover_markup(&thread)),
                range: Some(landed.range),
            }));
        }
        Ok(None)
    }

    /// The code actions for a selection in `uri` (`lens.compose`): a
    /// "Comment on these lines" action whose edit creates the compose
    /// template, anchored to exactly the selected lines against the working
    /// tree, and opens it (`workspace/applyEdit` — unlike
    /// `window/showDocument`, which Zed does not open local files for, a
    /// `CreateFile` edit is the mechanism clients already use to open a
    /// freshly created file, e.g. rust-analyzer's "Extract module to
    /// file"). Empty when the URI is not a file in the working tree.
    ///
    /// # Errors
    ///
    /// Never fails today; returns [`Result`] for symmetry with the other
    /// request handlers.
    pub fn code_actions(&self, uri: &Url, range: Range) -> Result<Vec<CodeActionOrCommand>> {
        let Some(workdir) = self.repo.workdir() else {
            return Ok(Vec::new());
        };
        let Some(rel) = document::relative_path(workdir, uri) else {
            return Ok(Vec::new());
        };
        let target = Target {
            path: Some(rel),
            lines: Some(selection_lines(range)),
            parent: None,
        };
        let edit = self.template_edit(&target)?;
        Ok(vec![CodeActionOrCommand::CodeAction(CodeAction {
            title: "Comment on these lines".to_owned(),
            kind: Some(CodeActionKind::EMPTY),
            diagnostics: None,
            edit: Some(edit),
            command: None,
            is_preferred: None,
            disabled: None,
            data: None,
        })])
    }

    /// Run a `workspace/executeCommand` the lens registered (`lens.lenses`,
    /// `lens.compose`): View returns the thread, Resolve/Reopen record the
    /// state mutation through the shared library call, and Reply opens a
    /// template (Compose does the same directly through its code action's
    /// edit, `lens.compose`).
    ///
    /// # Errors
    ///
    /// [`Error::BadArguments`] for a missing or malformed argument;
    /// otherwise propagates the underlying comment-store or template
    /// failure.
    pub fn execute_command(&self, command: &str, arguments: &[Value]) -> Result<Outcome> {
        match command {
            render::CMD_VIEW => {
                let id = arg_id(arguments)?;
                let comments = Comments::open(&self.repo);
                let thread = comments.thread(id)?;
                let markup = render::hover_markup(&thread);
                Ok(Outcome {
                    response: Some(json!(markup.value)),
                    ..Outcome::default()
                })
            }
            render::CMD_RESOLVE => {
                let id = arg_id(arguments)?;
                Comments::open(&self.repo).resolve(id)?;
                Ok(Outcome {
                    refresh: true,
                    ..Outcome::default()
                })
            }
            render::CMD_REOPEN => {
                let id = arg_id(arguments)?;
                Comments::open(&self.repo).reopen(id)?;
                Ok(Outcome {
                    refresh: true,
                    ..Outcome::default()
                })
            }
            render::CMD_REPLY => {
                let id = arg_id(arguments)?;
                let target = Target {
                    parent: Some(id.to_string()),
                    ..Target::default()
                };
                let edit = self.template_edit(&target)?;
                Ok(Outcome {
                    edit: Some(edit),
                    ..Outcome::default()
                })
            }
            other => Err(Error::BadArguments(format!("unknown command {other}"))),
        }
    }

    /// Handle a `textDocument/didSave`: if the saved file is a compose or
    /// reply template, finalize it (`lens.compose`); otherwise recompute
    /// diagnostics, since the saved buffer now matches disk.
    ///
    /// # Errors
    ///
    /// Propagates a template read or comment-creation failure.
    pub fn did_save(&self, uri: &Url) -> Result<Outcome> {
        if self.is_template(uri) {
            return self.finalize_compose(uri);
        }
        Ok(Outcome {
            refresh: true,
            ..Outcome::default()
        })
    }

    /// Diagnostics for `uri` even when the document is not open — the
    /// server uses this to clear or refresh a specific document.
    ///
    /// # Errors
    ///
    /// See [`Lens::diagnostics`].
    pub fn diagnostics_for(&self, uri: &Url) -> Result<Vec<Diagnostic>> {
        self.diagnostics(uri)
    }

    /// Whether `uri` names a compose/reply template under `.git/`
    /// (`lens.compose`): every template's filename starts with
    /// [`compose::TEMPLATE_PREFIX`], so this needs no state of its own to
    /// recognize one.
    fn is_template(&self, uri: &Url) -> bool {
        let Ok(saved) = uri.to_file_path() else {
            return false;
        };
        let Some(name) = saved.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        if !name.starts_with(compose::TEMPLATE_PREFIX) {
            return false;
        }
        let git_dir = self
            .repo
            .git_dir()
            .canonicalize()
            .unwrap_or_else(|_| self.repo.git_dir().to_owned());
        let saved_dir = saved
            .parent()
            .map(|parent| parent.canonicalize().unwrap_or_else(|_| parent.to_owned()));
        saved_dir.as_deref() == Some(git_dir.as_path())
    }

    /// The workspace edit that creates and fills in the compose/reply
    /// template for `target` under `.git/` (`lens.compose`): a `CreateFile`
    /// operation followed by a `TextDocumentEdit` inserting
    /// [`compose::template_text`] into it. Applying this edit
    /// (`workspace/applyEdit`) is what gets a client to open the template,
    /// since `window/showDocument` is not reliably supported for local
    /// files.
    ///
    /// # Errors
    ///
    /// [`Error::TemplateUri`] if the template path cannot be turned into a
    /// file URI.
    fn template_edit(&self, target: &Target) -> Result<WorkspaceEdit> {
        let path = self.repo.git_dir().join(compose::template_filename(target));
        let uri = document::file_uri(&path).ok_or_else(|| Error::TemplateUri(path.clone()))?;
        let create = DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
            uri: uri.clone(),
            options: Some(CreateFileOptions {
                overwrite: Some(true),
                ignore_if_exists: None,
            }),
            annotation_id: None,
        }));
        let insert = DocumentChangeOperation::Edit(TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier { uri, version: None },
            edits: vec![OneOf::Left(TextEdit {
                range: Range {
                    start: Position::default(),
                    end: Position::default(),
                },
                new_text: compose::template_text(target),
            })],
        });
        Ok(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(vec![create, insert])),
            ..WorkspaceEdit::default()
        })
    }

    /// Read the saved template at `uri` and create the comment it describes
    /// through the shared library call (`lens.parity`), anchoring to the
    /// working tree (`lens.working-tree`); an empty body aborts
    /// (`lens.compose`). The template is always removed afterward so a
    /// stale one is never reused.
    fn finalize_compose(&self, uri: &Url) -> Result<Outcome> {
        let template = uri
            .to_file_path()
            .unwrap_or_else(|()| self.repo.git_dir().to_owned());
        let content = std::fs::read_to_string(&template).map_err(|source| Error::Template {
            path: template.clone(),
            source,
        })?;
        let composed = compose::parse(&content);
        // Best effort: a leftover template is harmless — the next compose
        // overwrites it — so a removal failure never aborts a comment that
        // was otherwise created successfully.
        if let Err(_error) = std::fs::remove_file(&template) {}
        if composed.is_abort() {
            return Ok(Outcome::default());
        }

        let comments = Comments::open(&self.repo);
        if let Some(parent) = composed.target.parent {
            let parent_id = parse_id(&parent)?;
            comments.reply(parent_id, &composed.body, None)?;
        } else {
            let path = composed
                .target
                .path
                .ok_or_else(|| Error::BadArguments("compose template named no path".to_owned()))?;
            let lines = composed
                .target
                .lines
                .map(|text| parse_lines(&text))
                .transpose()?;
            let anchor = capture_worktree(&self.repo, &path, lines)?;
            comments.add(&Binding::Position(anchor), &composed.body, None)?;
        }
        Ok(Outcome {
            refresh: true,
            ..Outcome::default()
        })
    }
}

/// Whether `position`'s line falls within `range` — hovering anywhere on an
/// anchored line reveals its thread.
fn position_in(position: Position, range: Range) -> bool {
    position.line >= range.start.line && position.line <= range.end.line
}

/// The 1-based inclusive `<start>:<end>` line span a selection covers,
/// collapsing a trailing full-line boundary (`end` at column 0 of the next
/// line) back onto the last selected line.
fn selection_lines(range: Range) -> String {
    let start = range.start.line.saturating_add(1);
    let end = if range.end.character == 0 && range.end.line > range.start.line {
        range.end.line
    } else {
        range.end.line.saturating_add(1)
    };
    format!("{start}:{end}")
}

/// Parse a `<start>[:<end>]` line-span string into a [`gix_anchor::LineRange`].
fn parse_lines(text: &str) -> Result<gix_anchor::LineRange> {
    let bad = || Error::BadArguments(format!("not a line range: {text:?}"));
    let (start, end) = match text.split_once(':') {
        Some((start, end)) => (start, end),
        None => (text, text),
    };
    let start = start.trim().parse::<u64>().map_err(|_error| bad())?;
    let end = end.trim().parse::<u64>().map_err(|_error| bad())?;
    Ok(gix_anchor::LineRange { start, end })
}

/// Parse a hex object id, wrapping a malformed one in [`Error::InvalidId`].
fn parse_id(text: &str) -> Result<ObjectId> {
    ObjectId::from_hex(text.as_bytes()).map_err(|_error| Error::InvalidId(text.to_owned()))
}

/// Extract a single comment-id string argument.
fn arg_id(arguments: &[Value]) -> Result<ObjectId> {
    let text = arguments
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| Error::BadArguments("expected a comment id argument".to_owned()))?;
    parse_id(text)
}
