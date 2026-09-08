# File viewers

Files, mail attachments, and Telegram share one viewer for text, images, and
PDFs. Select a file in the browser, an attachment in a letter, or a document
filename in a Telegram conversation. Existing panel identities and source
verbs remain: the browser owns file operations, mail owns its parts, and
Telegram owns downloads and the walk between messages.

PDFs show one complete page, a page count, and previous/next page buttons.
Images and PDF pages fit inside the panel without cropping. Text is selectable
and scrolls within the reading area. The existing **open** verb still hands a
file to the operating system when another viewer is useful.

Each panel asks for its dimensions from the content it is showing. Short text
stays compact; long text asks for more height. Landscape images and PDF pages
ask for more width than portrait ones. The measurement is cached, updates when
content arrives or the PDF page changes, and is clamped to the workspace grid.

## Implementation

`app/src/shell/widgets/viewer/` owns file input, the shared widget, page
navigation, background workers, and content measurement. It names no app.
The file card embeds it; Telegram embeds it beside its source-specific
caption and feedback. Adapters supply bytes or a file reader, then copy the
measurement to their panel instance and request a relayout when it changes.

`app/src/reader/pdf.rs` uses Hayro to parse and rasterize PDF pages. A worker
parses a document once and rasterizes only the requested page. Its render
cache is local to that page, and its bitmap has a maximum edge of 2048 pixels.
The current texture is replaced on paging. Closing or replacing a source
drops its receiver, so an obsolete worker cannot populate a different file.
Headless builds perform the same work inline for deterministic UI tests.

`kernel/src/caps/preview.rs` owns the common input limits: text previews read
64 KiB, images accept up to 20 MiB, and PDFs accept complete files up to 64 MiB.
PDF reads include an extra byte to detect oversized files even when the source
has no size metadata. Malformed, unreadable, oversized, or password-protected
PDFs show a status message. Password entry, PDF text selection, annotations,
and editing are outside this viewer's current scope.

Mail downloads through its existing reader and bounded blob cache; after a
PDF is handed to the viewer it leaves the transient image cache. Telegram
uses the same source-refresh and cache download operation as attachment reads,
with progress and retry supplied by its existing feedback controls. Browsed
files use the window's disk factory on the viewer worker.

The build requires Rust 1.92 or newer, matching the PDF renderer; the project's
`mise` configuration uses stable Rust.
