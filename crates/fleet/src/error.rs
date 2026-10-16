#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),

    /// Exit with a specific code. The message (when non-empty) is printed as the
    /// error; an empty one means the command already said everything.
    #[error("{1}")]
    Exit(i32, String),

    /// A failure a `--json` caller can branch on: `kind` is a stable `snake_case` code
    /// (`dir_missing`, `name_taken`, …), the message is for people. Exits 1.
    #[error("{msg}")]
    Tagged { kind: &'static str, msg: String },

    /// A spawn that failed after its terminal was created: `partial` says where that terminal
    /// is (tmux session, window, pane; iTerm session), so the caller can find and close it.
    #[error("{source}")]
    Partial {
        source: Box<Error>,
        partial: Box<crate::core::backend::Spawned>,
    },
}

impl Error {
    pub fn exit(code: i32, msg: impl Into<String>) -> Self {
        Error::Exit(code, msg.into())
    }

    pub fn tagged(kind: &'static str, msg: impl Into<String>) -> Self {
        Error::Tagged {
            kind,
            msg: msg.into(),
        }
    }

    /// The same failure under a machine-readable `kind`; an already tagged one keeps its own.
    pub fn tag(self, kind: &'static str) -> Self {
        match self {
            Error::Tagged { .. } => self,
            Error::Partial { source, partial } => Error::Partial {
                source: Box::new(source.tag(kind)),
                partial,
            },
            e => Error::tagged(kind, e.to_string()),
        }
    }

    /// The machine-readable kind: the tagged one, else what the error is.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Tagged { kind, .. } => kind,
            Error::Partial { source, .. } => source.kind(),
            Error::Io(_) => "io",
            Error::Json(_) => "json",
            Error::Other(_) | Error::Exit(..) => "other",
        }
    }

    /// Where a failed spawn's terminal is, when it got that far.
    pub fn partial(&self) -> Option<&crate::core::backend::Spawned> {
        match self {
            Error::Partial { partial, .. } => Some(partial),
            _ => None,
        }
    }

    /// A partial failure knowing only the terminal's handle (the backend fills in the rest).
    pub(crate) fn with_partial_handle(self, handle: Option<String>) -> Self {
        Error::Partial {
            source: Box::new(self),
            partial: Box::new(crate::core::backend::Spawned {
                backend: crate::core::discovery::Backend::Unknown,
                desc: String::new(),
                tmux_session: None,
                tmux_window: None,
                handle,
                created_session: false,
                command: String::new(),
                prompt_file: None,
            }),
        }
    }

    /// The process exit code this error maps to.
    pub fn code(&self) -> i32 {
        match self {
            Error::Exit(c, _) => *c,
            _ => 1,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
