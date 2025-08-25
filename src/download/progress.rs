/*!
Shared progress message types used by both the TUI and the downloader implementation.

This module defines a minimal, transport-friendly set of messages that the
downloader emits over an async channel and the TUI consumes to update UI state.

Design notes:
- Keep messages small and self-contained to avoid tight coupling between the
  UI and downloader.
- Use `id` to correlate messages with a specific download row in the UI.
- `Started` may be sent more than once (e.g., initially with `None` total and
  again after the GET response reveals a `Content-Length`). The UI should
  overwrite the `total` when it receives a new `Started`.
- `Progress` reports incremental byte deltas, not absolute totals. The UI
  tracks the running sum per download to avoid reordering issues.
- `Renamed` is optional and can be used when the final filename is determined
  from headers (e.g., Content-Disposition). The UI may update the displayed
  filename accordingly.
*/

#[derive(Clone, Debug)]
pub enum ProgressMsg {
    /// Signals that a download is starting (or updating its known total).
    /// - `total`: the total size in bytes if known; may be `None` when the server
    ///   does not provide a content length.
    Started { id: usize, total: Option<u64> },

    /// An incremental progress update in bytes (delta, not absolute).
    Progress { id: usize, delta: u64 },

    /// Indicates the download has completed successfully.
    Finished { id: usize },

    /// Indicates the download failed with an error message.
    Failed { id: usize, err: String },

    /// Optional notification that the effective filename has changed (e.g., due
    /// to Content-Disposition header). The UI may update the displayed name.
    Renamed { id: usize, file: String },

    /// Indicates whether the download is resumable (e.g., Accept-Ranges: bytes).
    Resumable { id: usize, resumable: bool },
}
