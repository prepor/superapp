# Superapp launch film — revised cut

The 90-second film is `.context/promo/superapp-v3.mp4`; the chapter player is
`.context/promo/review-v3.html`. Its score is original, with a 120 BPM electronic
arrangement and sound effects aligned to the revised cuts. There is no voiceover.

## Edit

| Time | Picture | Added copy |
|---|---|---|
| 00–15 | Ten apps, 1.5 seconds each. Calendar shows 29 September events, then a day agenda. Telegram shows a cached photo in a chat and its full image viewer. Keep app names visible. | Mail, Files, Calendar, Workshop, Terminal, RSS, Notes, Agents, Telegram, Language learning |
| 15–19 | Establish the desktop. Its exact frame and position continue into the next section. | superapp / A userspace OS. |
| 19–31 | Search the launcher and switch to Calendar in another workspace; stack and split. Mail's tag completion filters to Max's threads, previews two messages and marks both. Files uses the same filter and preview, then stacks the PDF and switches tabs beside Calendar. | One UI. One way to work. / brief action and shortcut labels |
| 31–41 | Carry Launch checklist through 32 neighboring panels with repeated ⌘] and ⌘[. Start during the first movement. Real-time native animation. | Fast. / Makepad · Native GPU rendering |
| 41–47 | Three complete lists: email, Telegram chats, Workshop workspaces. Connect them to a single SQLite label. | One shared database. |
| 47–59 | A launch-review calendar event leads to the agent’s actual queries and a concise saved note. | Agents. With context. / Prep me for this meeting. / Across your apps. / Ready for the meeting. |
| 59–69 | The English article Berlin by bike becomes the actual lesson Ein Fahrrad, bitte. The article and first question both show “I’d like to rent a bike.” Answer it correctly. | Turn this into German practice. |
| 69–81 | Workshop and a Telegram photo chat beside the complete phone viewport. Open Overview, switch workspaces, drag a tile into a stack, swipe a tile closed, open the photo chat, create a new column, pan the overview strip and dismiss it. | Android, too. / Phone layout preview |
| 81–88 | Complete note viewports in two paired local instances. Change “Android clip: recording” to “ready” on Mac, then add “Ready to share.” from the phone layout. Both changes cross automatically. | Automatically in sync. / Two local instances · phone layout preview |
| 88–90 | App name alone. | superapp |

## Framing and review

Use the whole recorded viewport with `contain`, never an estimated panel crop
or a cover crop. Opening sources are native Retina viewports, downscaled without enlargement.
The desktop frame is fixed at (64, 224), 1792×806 in the 1920×1080 export. The
userspace-OS and One-UI scenes use the same first frame and exact rectangle.

A 60 fps MP4 alone proves nothing about motion. Native captures carry monotonic
draw timestamps. Check consecutive source-image content and the actual output
frames, especially during workspace changes and the continuous speed passage.
Keep the native animation clock, with no optical flow or synthesized in-between
frames. Review full-resolution frames, sequences around each action, and the
encoded result. Keep the original first cut and rejected takes for comparison.

The phone input uses `gesture-ms` to advance contact positions on animation
frames. The review follows the held tile's actual border across 38 consecutive
60 Hz samples and compares encoded motion. This catches an instantaneous
scripted drag even when the recorder itself writes frames at a high rate.

The Telegram attachment is a generated sample photograph. Its PNG and the exact
built-in imagegen prompt are saved in `.context/promo/v3/assets/`. It is loaded
by the app's native photo widgets and file viewer, not painted over a screenshot.

## Agent evidence

The meeting run reads `calendar.event`, `mail.search`, `mail.thread`, and SQL
queries over cached Telegram messages. It saves `Launch prep` through
`notes.create`. All calls in the accepted run completed successfully. The note
contains the meeting time, documentation status, Android-clip status, and three
meeting questions. The result viewport opens the saved note explicitly.

The language run reads the attached RSS record through SQL, calls `fluent.due`,
and uses `fluent.author` to save five A2 exercises and two vocabulary cards.
Its first closed question has the answer “Ich möchte ein Fahrrad mieten.” The
result capture actually selects it and shows “Richtig!” Processing waits are
removed; the film does not make a model-speed claim.

The source databases and `result.json` records live under
`.context/promo/v2/live-meeting/` and `live-language/`. All records used in the
film are prepared demo data. Ordinary accounts and the everyday store are not
capture inputs.

## Device status

The connected phone was locked and subsequently disconnected. The Android
section therefore uses a labelled native Mac preview at phone dimensions,
including the app’s touch/Overview paths. It is not physical Android footage.
The sync section records two separate local stores over loopback, with the same
note UID verified at both ends. It does not establish Mac-to-Android latency.
An unlocked connected device is still needed for those two physical-device takes.

## Capture implementation

The background windows receive process-local input through Makepad’s remote
bridge. Foreground checks accompany controls; there are no global key events.
ScreenCaptureKit dropped updates from covered windows. The optional native
capture hook uses Makepad’s Metal readback and logs real draw times. The private
capture build can write uncompressed BMP frames to avoid PNG compression in the
GPU completion callback. `promo/build_capture.py` patches a private Cargo
checkout under `.context`, leaving the shared dependency checkout unchanged.

See [production utilities](../promo/README.md) for the commands and generated
artifacts. The ending intentionally has no open-source text or repository URL.
