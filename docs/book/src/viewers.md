# File viewers

Files, mail attachments, and Telegram share one viewer for text, images, and
PDFs. Select a file in the browser, an attachment in a letter, or a document
filename in a Telegram conversation. Existing panel identities and source
verbs remain: the browser owns file operations, mail owns its parts, and
Telegram owns downloads and the walk between messages.

PDFs open as a continuous vertical document, initially fitted to the panel's
width. Scroll or drag to read the next page; the position line and scrollbar
show where you are. Pages keep their own proportions, including mixed sizes
and rotation. Images initially fit in full. Text is selectable and scrolls
within the reading area. The **open** verb hands the file to the system.

Pinch on a trackpad or touchscreen to zoom around the pointer or fingers;
drag or scroll to pan. The panel's verb bar offers **fit page** (or **fit
image**), **fit width**, **zoom in**, and **zoom out**. **Cmd+F** fits the current
page or image, resetting both scale and panning. Fit width restores the whole
document width while keeping the current page in view. Previous/next page verbs
jump within the continuous document; internal links do the same. Fit modes
adapt when the panel resizes, preserving the reading position after scrolling.
A new file starts at its initial fit, and zoom or scrolling never changes the
panel's dimensions.

PDF link annotations are clickable: web and email links open through the
system, and links to pages or named destinations jump within the document.
The hand cursor and click targets follow the page through crop, rotation,
zoom, and pan. A drag or pinch over a link never follows it.

Each panel asks for its dimensions from the content it is showing. Short text
stays compact; long text asks for more height. Landscape images and PDF pages
ask for more width than portrait ones. PDFs reserve a tall reading panel while
loading, then use the first page's orientation; the dimensions stay stable
through scrolling. Metadata arrives before page rendering and updates layout
without waiting for another input event. All wishes are clamped to the grid.

## Implementation

`app/src/shell/widgets/viewer/` owns file input, the shared widget, page
navigation, background workers, and content measurement. It names no app.
The file card embeds it; Telegram embeds it beside its source-specific
caption and feedback. Each panel owns a `Controller`, binds its widget with
`FileViewerRef::bind`, appends `Controller::verbs()` to its bar, and delegates
viewer verbs to `Controller::run()` followed by the usual session redraw.
The widget publishes measurements and reading state through the controller;
`Panel::wish` reads `Controller::measure()`. Loading and verb commands request
another draw explicitly when they finish during a draw, and the stage settles
layout changes from hosted content before becoming idle.
The viewer canvas uses one transform for the image, clipped link hits, and
gesture coordinates. It claims its touch updates through the hosted widget's
`Grab`, so the shell does not also pan the workspace or open an overlay;
trackpad movement uses the scroll event's handled flags for the same reason.

`app/src/reader/pdf.rs` uses Hayro to parse and rasterize PDF pages. A worker
parses a document once and publishes all page dimensions before rasterizing.
The canvas places every page, requests visible pages first, and preloads nearby
pages within a 96 MiB texture working set. Individual bitmaps are at most
32 MiB with a maximum edge of 4096 pixels, at up to four pixels per PDF point.
Only one render runs at a time; gesture events transform existing textures.
An unavailable page keeps its place and shows its error without hiding other
pages. Closing or replacing a source drops its receiver, so an obsolete
worker cannot populate a different file. Headless builds do the same work
inline for deterministic UI tests.
`app/src/reader/pdf/links.rs` resolves link annotations and converts their
rectangles with the renderer's crop and rotation transform. It accepts URI
actions for HTTP, HTTPS, and email, and local page destinations; executable
actions and links to local files are ignored.

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
