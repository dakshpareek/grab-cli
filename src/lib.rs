#![doc = "Library entry point exposing modules for binaries (main and TUI)."]

// Re-export the download module so bins can use `crate::download::*`
pub mod download;
