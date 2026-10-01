//! Protocol between the UI and the agent: a stream of frames, the same over a
//! local socket as over SSH. Each frame is a `u32` length (little endian)
//! followed by the message in MessagePack.

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Bumped only for an incompatible change. Adding requests, responses or
/// events isn't one: they are ALWAYS ADDED AT THE END of their enum, and
/// whoever receives something unknown rejects it without dropping the
/// connection (see `Decoded`). Each version has its own socket, so an agent of
/// another version is never shut down: it keeps its terminals until they're gone.
pub const PROTOCOL: u32 = 6;

/// Maximum frame size, so garbage input can't make us allocate without limit.
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Largest file payload, leaving room for the response envelope in a frame.
pub const MAX_FILE_BYTES: usize = MAX_FRAME - 1024;

#[derive(Debug)]
pub struct FrameTooLarge(pub usize);

impl std::fmt::Display for FrameTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "frame too large: {} bytes (maximum {MAX_FRAME})", self.0)
    }
}

impl std::error::Error for FrameTooLarge {}

pub type TermId = u64;

/// Message from the UI to the agent. Without an `id` no response is expected.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClientMessage {
    pub id: Option<u64>,
    pub request: Request,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Hello { protocol: u32 },
    /// Terminates the agent and all its terminals.
    Shutdown,
    /// `group` groups terminals (today, the project folder; later, the task).
    TermCreate {
        group: String,
        cwd: PathBuf,
        command: Option<Vec<String>>,
        cols: u16,
        rows: u16,
    },
    TermList { group: String },
    /// Responds with the terminal's snapshot; its `TermOutput`s follow.
    TermAttach { term: TermId },
    TermDetach { term: TermId },
    TermInput { term: TermId, #[serde(with = "serde_bytes")] data: Vec<u8> },
    TermResize { term: TermId, cols: u16, rows: u16 },
    TermKill { term: TermId },
    /// Current directory of the foreground process.
    TermCwd { term: TermId },
    /// Saves a pasted image to a temporary file on the agent's machine and
    /// responds with its path, to paste it into the terminal.
    SavePastedImage { extension: String, #[serde(with = "serde_bytes")] data: Vec<u8> },
    /// Adds a git repo (or a worktree's repo) to those the agent knows.
    RepoAdd { path: PathBuf },
    /// Forgets a repo (touches nothing on disk).
    RepoRemove { path: PathBuf },
    /// Repos the agent knows.
    RepoList,
    /// Tasks from all known repos: their worktrees, the main one included.
    TaskList,
    /// Creates a task in the repo of `repo` (a worktree works too): runs
    /// `.sik/create <name>` if it exists and, otherwise, `git worktree add`.
    /// May take a while; the response arrives when it's done. With `open`, the
    /// connected UIs open it (`sik task` from a sik terminal).
    TaskCreate { repo: PathBuf, name: String, open: bool },
    /// Removes the task (its worktree) with the repo's `.sik/remove` if it
    /// exists and, otherwise, with `git worktree remove` (which refuses if
    /// there are uncommitted changes). Closes its terminals. May take a while.
    TaskRemove { path: PathBuf },
    /// What the task at `path` has changed against its base branch (or, with
    /// `uncommitted`, only what isn't committed yet), not counting ignored files.
    GitChanges { path: PathBuf, uncommitted: bool },
    /// Unified diff of `file` (relative to `path`) using the same criteria.
    GitDiff { path: PathBuf, file: String, uncommitted: bool },
    /// All files under `path` (relative), without what git ignores.
    FindFiles { path: PathBuf },
    /// Searches for `query` in the files under `path`, without what git ignores.
    Search {
        path: PathBuf,
        query: String,
        regex: bool,
        case_sensitive: bool,
        max_hits: usize,
    },
    ReadFile { path: PathBuf },
    WriteFile { path: PathBuf, #[serde(with = "serde_bytes")] data: Vec<u8> },
    /// A folder without what git ignores: folders first, by name.
    ListDir { path: PathBuf },
    Rename { from: PathBuf, to: PathBuf },
    CreateFile { path: PathBuf },
    CreateDir { path: PathBuf },
    /// To the Trash, not deleted.
    Trash { path: PathBuf },
    /// Reports changes inside `path` to this connection with `FsChanged`.
    Watch { path: PathBuf },
    Unwatch { path: PathBuf },
    /// Fingerprint of the agent binary (`build_id`), to know whether there's a new one.
    Version,
    /// A git operation on the repo at `path` (the task folder).
    Git { path: PathBuf, op: GitOp },
    /// Asks the language server of file `path` (in the task at `root`) about
    /// what's at `line`, `column` (0-based, in characters).
    /// `text` is the editor's text, saved or not. Responds `Lsp`.
    Lsp {
        root: PathBuf,
        path: PathBuf,
        text: String,
        line: u32,
        column: u32,
        op: LspOp,
    },
    /// Tasks (groups) waiting for an answer right now. Responds `Files`.
    BlockedList,
    /// Replaces what `Search` with the same `query`, `regex` and
    /// `case_sensitive` finds in `files` (relative to `path`) with
    /// `replacement` (`$1`… with `regex`). With `preserve_case`, each
    /// replacement takes the case of what it replaces. Responds `Replaced`.
    Replace {
        path: PathBuf,
        files: Vec<String>,
        query: String,
        regex: bool,
        case_sensitive: bool,
        replacement: String,
        preserve_case: bool,
    },
    /// TCP ports that processes started from the terminals listen on.
    /// Responds `Ports`.
    Ports,
    /// The detail and documentation of completion `item` of `list` (a
    /// `Completions` for file `path` in the task at `root`), which some
    /// servers only give when asked for one. Responds `Resolved`.
    LspResolve { root: PathBuf, path: PathBuf, list: u64, item: u32 },
    /// `text`, the editor's, formatted for file `path` in the task at
    /// `root`. Responds `Formatted`.
    Format { root: PathBuf, path: PathBuf, text: String },
    /// `sik <path>` in a terminal: asks the UIs connected to this agent to
    /// open `root` as a workspace, with `file` in it. Responds `Count`: how
    /// many other connections it was sent to (none: no app is listening).
    Open { root: PathBuf, file: Option<PathBuf> },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LspOp {
    Definition,
    References,
    /// Responds `Completions`.
    Completion,
    /// Responds `Signature`.
    SignatureHelp,
}

/// The signature of the call the cursor is in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspSignature {
    pub label: String,
    /// The parameter the cursor is on: its start and end in `label`, in
    /// characters.
    pub active: Option<(u32, u32)>,
    /// That parameter's documentation or, without one, the signature's;
    /// plain text.
    pub documentation: Option<String>,
}

/// A completion the language server offers at the cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspCompletion {
    pub label: String,
    /// LSP `CompletionItemKind`.
    pub kind: Option<u32>,
    /// The type or signature, shown next to the label.
    pub detail: Option<String>,
    /// Markdown.
    pub documentation: Option<String>,
    /// What replaces the text from `start` to the cursor.
    pub text: String,
    /// 0-based column, in characters, on the cursor's line.
    pub start: u32,
    /// What the typed text is matched against.
    pub filter: String,
    /// What orders it among the others.
    pub sort: String,
}

/// A code location returned by the language server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspLocation {
    pub path: PathBuf,
    /// 0-based; the column and length, in characters of the line.
    pub line: u32,
    pub column: u32,
    pub length: u32,
    /// The whole line, for display.
    pub text: String,
}

/// Git operations of the Changes mode. Files are relative to the task
/// folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GitOp {
    /// Responde `GitStatus`.
    Status,
    Stage { files: Vec<String> },
    Unstage { files: Vec<String> },
    /// Reverts unstaged changes; untracked files go to the Trash.
    Discard { files: Vec<String> },
    /// With `all`, stages everything changed first.
    Commit { message: String, all: bool },
    /// Responde `Branches`.
    Branches,
    /// Switches to the local `branch`.
    Switch { branch: String },
    /// Responds `Commits`, starting at `HEAD`.
    Log { skip: usize, limit: usize },
    /// Responds `Changes`: what the commit changed against its first parent.
    CommitFiles { commit: String },
    /// Responds `Text`: the diff of `file` in the commit.
    CommitDiff { commit: String, file: String },
    /// Responds `Text`: the whole commit, its header, message and diff.
    Show { commit: String },
    /// Responds `Text`: `file` as it was in the commit (before it, if the
    /// commit deleted it).
    FileAt { commit: String, file: String },
    /// Responds `Commits`: those of every local branch and tag whose hash,
    /// message or author contain each word of `query`.
    Search { query: String, skip: usize, limit: usize },
    /// Responds `Blame` for `file` as it is on disk.
    Blame { file: String },
    /// Responds `Text`: the unified diff of `file` with the whole file as
    /// context, for the side-by-side view. In `commit` if there is one, and
    /// otherwise as `Request::GitDiff` does.
    WholeDiff { file: String, commit: Option<String>, uncommitted: bool },
    /// Responds `Commits`: those that changed `file`, from `HEAD`.
    FileLog { file: String, skip: usize, limit: usize },
}

/// Repo status for the Changes mode, uncommitted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GitStatus {
    /// `None` with a detached `HEAD`.
    pub branch: Option<String>,
    /// In the index, ready to commit.
    pub staged: Vec<ChangedFile>,
    /// In the folder and unstaged, untracked files included.
    pub unstaged: Vec<ChangedFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// Seconds since 1970.
    pub time: i64,
    /// Branches and tags pointing at it, as `git log --decorate` shows them.
    pub refs: String,
    pub subject: String,
}

/// Message from the agent to the UI.
#[derive(Debug, Serialize, Deserialize)]
pub enum ServerMessage {
    Response { id: u64, result: Result<Response, String> },
    Event(Event),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Hello { protocol: u32, pid: u32 },
    Ok,
    TermCreated { term: TermId },
    TermList(Vec<TermInfo>),
    /// Escape sequences that reproduce the screen, history, cursor and modes
    /// in a fresh `cols` × `rows` emulator.
    TermSnapshot { cols: u16, rows: u16, #[serde(with = "serde_bytes")] data: Vec<u8> },
    Path(Option<PathBuf>),
    Tasks(Vec<TaskInfo>),
    Repos(Vec<PathBuf>),
    Task(TaskInfo),
    Changes { base: Option<String>, files: Vec<ChangedFile> },
    Text(String),
    Files(Vec<String>),
    SearchResults { hits: Vec<SearchHit>, truncated: bool },
    Bytes(#[serde(with = "serde_bytes")] Vec<u8>),
    Dir(Vec<DirEntryInfo>),
    GitStatus(GitStatus),
    Branches { current: Option<String>, branches: Vec<String> },
    Commits(Vec<CommitInfo>),
    /// `server` is the server that responded; `None` if there's none for that
    /// language (and then `locations` is empty).
    Lsp { server: Option<String>, locations: Vec<LspLocation> },
    /// Who last changed each line of a file: `lines[i]` is the index in
    /// `commits` of line `i`'s commit, or `None` if it isn't committed.
    Blame { commits: Vec<CommitInfo>, lines: Vec<Option<u32>> },
    /// How many replacements were made in how many files.
    Replaced { files: usize, replacements: usize },
    Ports(Vec<PortInfo>),
    /// `server` as in `Lsp`. `incomplete`: typing more may bring others,
    /// so they're asked again instead of filtering these. `list` names them
    /// for `LspResolve`.
    Completions { server: Option<String>, list: u64, items: Vec<LspCompletion>, incomplete: bool },
    /// What `LspResolve` adds to a completion; `None` what the server didn't say.
    Resolved { detail: Option<String>, documentation: Option<String> },
    /// `None` outside a call.
    Signature(Option<LspSignature>),
    /// `None` if nothing formats that kind of file; `by` says what did.
    Formatted { text: Option<String>, by: Option<String> },
    Count(usize),
}

/// A port a process started from a terminal listens on (at loopback or on
/// every address).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortInfo {
    pub port: u16,
    /// Group of the terminal that started it (its task).
    pub group: String,
    /// Name of the process listening.
    pub process: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirEntryInfo {
    pub name: String,
    pub is_dir: bool,
}

/// A changed file in a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// Relative to the task folder.
    pub path: String,
    /// `A` added, `M` modified, `D` deleted, `?` untracked, `U` conflicted.
    pub status: char,
    pub added: u32,
    pub removed: u32,
}

/// A search match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Relative to the search folder.
    pub path: String,
    /// 1-based, as in editors.
    pub line: u32,
    /// Column (in characters, 0-based) and length of the match.
    pub column: u32,
    pub length: u32,
    pub text: String,
}

/// A task is a worktree of a known repo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskInfo {
    /// Folder of the repo's main checkout.
    pub repo: PathBuf,
    /// Worktree folder; also the group of its terminals.
    pub path: PathBuf,
    pub branch: Option<String>,
    /// It's the main checkout, not an added worktree.
    pub main: bool,
    /// One of its terminals is producing output.
    pub working: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermInfo {
    pub term: TermId,
    pub title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Event {
    TermOutput { term: TermId, #[serde(with = "serde_bytes")] data: Vec<u8> },
    TermTitle { term: TermId, title: Option<String> },
    TermExit { term: TermId },
    /// A task started or stopped working (its terminals produce output). Sent
    /// to every connection, whether attached to its terminals or not.
    Activity { group: String, working: bool },
    /// Asks the UI to open this newly created task.
    OpenTask { path: PathBuf },
    /// Something changed inside a folder watched with `Watch`.
    FsChanged { root: PathBuf, paths: Vec<PathBuf> },
    /// A task started or stopped waiting for an answer (Claude is asking something).
    /// Sent to every connection, like `Activity`.
    Blocked { group: String, blocked: bool },
    /// Asks the UI to open `root` as a workspace, with `file` in it (see
    /// `Request::Open`).
    Open { root: PathBuf, file: Option<PathBuf> },
}

/// Fingerprint of a binary (64-bit FNV-1a): tells agent versions apart.
pub fn build_id(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ *byte as u64).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

pub fn write_frame<T: Serialize>(writer: &mut impl Write, message: &T) -> Result<()> {
    let body = rmp_serde::to_vec(message)?;
    // Reject before writing even the header, so the next frame is still readable.
    if body.len() > MAX_FRAME {
        return Err(FrameTooLarge(body.len()).into());
    }
    writer.write_all(&(body.len() as u32).to_le_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

/// What was read from a frame that may carry something this side doesn't know.
pub enum Decoded<T> {
    Known(T),
    /// A message from a newer version. `id` is its request, if any, so it can be
    /// answered (or treated as failed) without dropping the connection.
    Unknown { id: Option<u64> },
}

/// Like `read_frame`, but a frame that can't be understood isn't an error.
pub fn read_message<T: DeserializeOwned, Envelope: DeserializeOwned + MessageId>(
    reader: &mut impl Read,
) -> Result<Option<Decoded<T>>> {
    let Some(body) = read_body(reader)? else {
        return Ok(None);
    };
    if let Ok(message) = rmp_serde::from_slice::<T>(&body) {
        return Ok(Some(Decoded::Known(message)));
    }
    let id = rmp_serde::from_slice::<Envelope>(&body).ok().and_then(|envelope| envelope.id());
    Ok(Some(Decoded::Unknown { id }))
}

/// The request a message belongs to, read without understanding the rest.
pub trait MessageId {
    fn id(&self) -> Option<u64>;
}

/// `ClientMessage` with the request left unread.
#[derive(Deserialize)]
pub struct ClientEnvelope {
    id: Option<u64>,
    #[allow(dead_code)]
    request: serde::de::IgnoredAny,
}

impl MessageId for ClientEnvelope {
    fn id(&self) -> Option<u64> {
        self.id
    }
}

/// `ServerMessage` with the response and event left unread.
#[derive(Deserialize)]
pub enum ServerEnvelope {
    Response {
        id: u64,
        #[allow(dead_code)]
        result: serde::de::IgnoredAny,
    },
    Event(serde::de::IgnoredAny),
}

impl MessageId for ServerEnvelope {
    fn id(&self) -> Option<u64> {
        match self {
            ServerEnvelope::Response { id, .. } => Some(*id),
            ServerEnvelope::Event(_) => None,
        }
    }
}

fn read_body(reader: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        bail!("frame too large: {len} bytes");
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Reads a frame; `None` if the stream closed cleanly between frames.
pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        bail!("frame too large: {len} bytes");
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(Some(rmp_serde::from_slice(&body).context("invalid frame")?))
}

/// The app's state directory (the agent's socket and log).
pub fn state_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SIK_STATE_DIR") { return Ok(path.into()); }
    let base = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .context("no state directory")?;
    Ok(base.join(APP))
}

/// The app's config directory.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SIK_CONFIG_DIR") { return Ok(path.into()); }
    Ok(dirs::config_dir().context("no config directory")?.join(APP))
}

/// The local agent's socket identity. Windows maps it to a per-user named pipe;
/// the filesystem path also locates restart state.
/// `SIK_AGENT_SOCKET` overrides it (tests, or a separate development agent).
pub fn socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SIK_AGENT_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    Ok(state_dir()?.join(format!("agent-{PROTOCOL}.sock")))
}

/// App name: paths and binaries derive from it.
pub const APP: &str = "sik";

/// Where the app writes its own binary's path on starting, for `sik <path>`
/// to start it when it isn't running (the agent runs from a copy, elsewhere).
pub fn app_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("app"))
}

/// What `sik <path>` opens: a folder as it is; a file in its repo (the
/// folder of the nearest `.git` above it), or in its own folder outside one.
/// `path` is absolute.
pub fn open_target(path: &std::path::Path) -> (PathBuf, Option<PathBuf>) {
    if path.is_dir() {
        return (path.to_path_buf(), None);
    }
    let parent = path.parent().unwrap_or(path);
    let root = parent.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(parent);
    (root.to_path_buf(), Some(path.to_path_buf()))
}

/// Name of the agent binary.
pub const AGENT_BIN: &str = if cfg!(windows) { "sik-agent.exe" } else { "sik-agent" };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_payloads_are_compatible_with_legacy_byte_arrays() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Legacy { data: Vec<u8> }
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Binary { #[serde(with = "serde_bytes")] data: Vec<u8> }
        let data = vec![0, 127, 128, 255];
        let legacy = Legacy { data: data.clone() };
        let binary = Binary { data };
        assert_eq!(rmp_serde::from_slice::<Legacy>(&rmp_serde::to_vec(&binary).unwrap()).unwrap(), legacy);
        assert_eq!(rmp_serde::from_slice::<Binary>(&rmp_serde::to_vec(&legacy).unwrap()).unwrap(), binary);
    }

    #[test]
    fn largest_file_payload_fits_in_a_frame() {
        let message = ServerMessage::Response {
            id: u64::MAX,
            result: Ok(Response::Bytes(vec![255; MAX_FILE_BYTES])),
        };
        let mut frame = Vec::new();
        write_frame(&mut frame, &message).unwrap();
        assert!(frame.len() <= MAX_FRAME + 4);
        let Some(ServerMessage::Response { result: Ok(Response::Bytes(data)), .. }) =
            read_frame(&mut frame.as_slice()).unwrap() else { panic!("expected file bytes") };
        assert_eq!(data.len(), MAX_FILE_BYTES);
        assert!(data.iter().all(|&byte| byte == 255));
    }

    #[test]
    fn oversized_output_does_not_corrupt_the_stream() {
        let mut frame = Vec::new();
        write_frame(&mut frame, &Response::Ok).unwrap();
        let before = frame.clone();
        let error = write_frame(&mut frame, &Response::Text("x".repeat(MAX_FRAME))).unwrap_err();
        assert!(error.is::<FrameTooLarge>());
        assert_eq!(frame, before);
        write_frame(&mut frame, &Response::Ok).unwrap();
        let mut reader = frame.as_slice();
        assert!(matches!(read_frame::<Response>(&mut reader).unwrap(), Some(Response::Ok)));
        assert!(matches!(read_frame::<Response>(&mut reader).unwrap(), Some(Response::Ok)));
        assert!(read_frame::<Response>(&mut reader).unwrap().is_none());
    }

    #[test]
    fn frames_round_trip() {
        let message = ClientMessage {
            id: Some(7),
            request: Request::TermInput {
                term: 3,
                data: b"ls\r".to_vec(),
            },
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &message).unwrap();
        write_frame(&mut buf, &message).unwrap();
        let mut reader = buf.as_slice();
        for _ in 0..2 {
            let read: ClientMessage = read_frame(&mut reader).unwrap().unwrap();
            assert_eq!(read.id, Some(7));
            assert!(matches!(read.request, Request::TermInput { term: 3, .. }));
        }
        assert!(read_frame::<ClientMessage>(&mut reader).unwrap().is_none());
    }

    /// A message from a newer version is recognized as unknown, with its id,
    /// and the next frame reads fine.
    #[test]
    fn unknown_messages_keep_the_stream_alive() {
        #[derive(Serialize)]
        enum NewRequest {
            #[allow(dead_code)]
            Hello { protocol: u32 },
            FromTheFuture { x: u32 },
        }
        #[derive(Serialize)]
        struct NewMessage {
            id: Option<u64>,
            request: NewRequest,
        }
        let mut buf = Vec::new();
        write_frame(&mut buf, &NewMessage { id: Some(9), request: NewRequest::FromTheFuture { x: 1 } }).unwrap();
        write_frame(&mut buf, &ClientMessage { id: Some(10), request: Request::TaskList }).unwrap();
        let mut reader = buf.as_slice();
        match read_message::<ClientMessage, ClientEnvelope>(&mut reader).unwrap() {
            Some(Decoded::Unknown { id }) => assert_eq!(id, Some(9)),
            _ => panic!("should have been unknown"),
        }
        match read_message::<ClientMessage, ClientEnvelope>(&mut reader).unwrap() {
            Some(Decoded::Known(message)) => assert_eq!(message.id, Some(10)),
            _ => panic!("should have been read"),
        }
    }
}
