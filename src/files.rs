//! How files change on disk: file operations and their jobs, the trash, the one way each kind of
//! write is made, Glimt's own files and their backup, Rename Many's journal, and the external
//! programs and desktop portals that do work on files.

pub(crate) mod backup;
pub(crate) mod config;
pub(crate) mod external;
pub(crate) mod file_io;
pub(crate) mod file_ops;
pub(crate) mod journal;
#[cfg(feature = "portals")]
pub(crate) mod portal;
pub(crate) mod trash;
