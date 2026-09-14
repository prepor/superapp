# Vocabulary

- **Kernel**: everything generic that does not draw. The panel model,
  navigation, the store, effects, undo history, the rich table's state, and
  the interfaces an app implements. It never names Makepad and never names an
  app.
- **Shell**: everything generic that draws or takes input. The stage, the
  chrome, the bar, animation, the overlays, the shared widgets, and the
  hosting of panel widgets. It uses Makepad and never names an app.
- **App**: mail, files, agents, or the shell's own `system` app. It implements
  the kernel's interfaces and supplies its widgets to the shell. See
  [Apps](./apps.md).
- **Panel**: one focused view. What it shows is its **panel identity**; where
  it sits is its **slot**.
- **Panel identity**: a **tag** and its arguments, as in `inbox`,
  `message(42)`, `attachment(42, 3)`. Two slots may show the same identity.
- **Tag**: the name of a panel kind, as a plain word such as `inbox`,
  `files`, or `help`. It is what the store keeps and never changes once written.
- **Slot**: a place in a column holding one live panel instance. Its number
  is what joins, focus, and history refer to.
- **Workspace**: a horizontally scrolling row of columns.
- **Column**: a vertical stack of slots. All columns are equal; there is no
  "main" column.
- **Grid**: the units used to size panels. Desktop uses 12×6. A panel
  requests a width and height within the grid.
- **Wish**: a panel size calculated from its content. The layout clamps it to
  the active grid.
- **Verb**: one entry of a panel's bar: a button the panel runs by its id, or
  a link that navigates.
- **Bar**: the row of verbs at a panel's foot. It is pulled from the panel on
  every draw and it is where every accelerator lives.
- **Mark**: a row selected for a batch action. Marks use stable row keys, so
  they survive filtering, paging, and updates from a worker.
- **Join**: a relationship between a panel and a child opened from it. A new
  joined child from the same parent replaces the old child.
- **Bridge**: the ═ symbol between two joined panels.
- **Chain**: a join of a join (inbox → message → contact). Replacing or
  closing a panel closes its chain.
- **Camera**: the visible horizontal position of a workspace. It follows
  focus and moves directly with trackpad input.
- **Scene**: the target position and size of each panel, plus the target
  camera position. The shell animates toward these targets.
- **Ghost**: the fading image of a panel after it closes.
- **Toast**: a short status message in the bottom-right corner.
- **Problem**: a standing background condition, such as a failed provider sync,
  a failed send or a refused device-sync operation. An offline peer is ordinary
  status on the Device Sync panel. The
  **mark** in the toast's corner counts them; the problems panel lists them.
- **Effect**: work outside the database, such as using the network, keychain,
  clipboard, clock, or disk.
- **Capability**: the trait an effect reaches the outside through: `Clock`,
  `Secrets`, `Clipboard`, `Screen`, `Disk`, `Watcher`, `Speech`, the senses,
  and whatever an app defines for itself. A world is given one implementation
  of each.
- **Sense**: a capability that is what the machine *perceives* — `Location`,
  where the device is, and `Capture`, what its camera sees and its microphone
  hears. The real ones are the platform's; every test, scripted run and
  library mount gets the fakes. See [Architecture](./architecture.md#the-senses).
- **Fix**: one reading of where the device is: latitude, longitude, accuracy
  in metres, the heading while it moves, and when it was taken.
- **Capture**: a file the camera or the microphone made for an app — a photo,
  a voice note, or a video message. It sits under `captures/` beside the store
  until it is sent or discarded.
- **Live share**: a location message a worker keeps moving — editing it with a
  newer fix — until its period ends or the person stops it.
- **Job**: an effect saved in the database so it can be retried. The Effects
  panel shows jobs together with recent effects that were not saved.
- **Worker**: an asynchronous service with its own world and store reader: a
  mail account's sync, the sender, a chat's run. An app says which it wants
  running, and the kernel keeps the set in step with the store.
- **Agent**: what answers in a chat. [Agents](./agents.md) uses a model behind
  the gateway and app tools; [Workshop](./workshop.md) runs local harnesses
  around a Git worktree.
- **Chat**: a conversation, shown as a panel. Telegram, Agents and Workshop
  each own their conversation records and identities.
- **Turn**: one message of a chat, a person's or the agent's. The wire's own
  message, kept verbatim, and what the next request is built from.
- **Chip**: a piece of context put into a chat as a thing rather than as text.
  A panel, today: its identity, not a snapshot, so its rows are read again when
  the request goes out.
- **Run**: one round of the agent working on a chat — a request, the tool calls
  it asks for, their results — until it stops. A row, driven by a worker.
- **Tool**: one thing an app lets an agent do, by name, with a JSON schema:
  the verb's own code path over ids instead of over a cursor.
- **Call**: one use of a tool inside a run. A row, so the chat panel can run it
  on the UI thread and the transcript can show what it came to. In
  [Telegram](./telegram.md#calls) the same word is the other thing it has
  always meant: one conversation over the wire with one person, audio or
  video, in one of the states the wire names.
- **Gateway**: where a request goes — a Cloudflare AI Gateway in front of the
  model, opened with the token the backup form already holds.
