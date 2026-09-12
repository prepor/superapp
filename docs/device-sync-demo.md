# Device sync: pairing two devices

Two devices, two stores, one connection. There is no server to stand up and no
bucket to point at: a device's identity is a key it makes on its first open,
and pairing is one long string carried from the device that shows it to the
device that pastes it.

[The chapter](./book/src/device-sync.md) is what this is; below is how to see
it happen — first as two processes on this machine, then as a laptop and a
phone.

## 1. Build

```sh
mise exec -- cargo build -p superapp --no-default-features
```

`--no-default-features` leaves out the native Telegram dependency, so nothing
has to be installed for any of this.

## 2. Two processes on one machine

The scripted version of the whole walk, about ten seconds:

```sh
MAKEPAD=headless mise exec -- cargo build -p superapp --no-default-features
./e2e/sync/pair.sh
```

It runs two headless processes on two temporary stores, both with
`SUPERAPP_SYNC=loopback`, which is what makes a scripted run bind an endpoint
at all and binds it on `127.0.0.1` with no relay. A opens *device sync* and its
ticket is written to a file; B pastes it and presses **pair**. A note written
on each has to appear in the other's list, and both stores are then asked, in
SQL, whether their rosters name the same two devices.

Two windows on one Mac cannot stand in for two devices: a device's key lives in
the login keychain under one service and one account, so a second process on
the same machine reads the same `sync/key`, is the same device, and has nothing
to pair with. The walk above works because a scripted run keeps its secrets in
memory and each process makes a key of its own. For two windows, use two
machines — or the phone below.

## 3. A laptop and a phone

Two real devices, and the ticket carried between them by hand. It is a long
string, so the practical road is Telegram's **saved messages**:

1. On the laptop: **cmd cmd**, type `device sync`, enter. The panel shows this
   device's name, its short id, and a **ticket**. Press **copy**.
2. Paste it into saved messages, from any client.
3. On the phone, open saved messages, copy the string, open *device sync*
   there, paste it into **pair with**, and press **pair**.

Within a second each panel lists the other as *connected*, and each store's
roster holds both — pairing is an ordinary write to `sync_peer`, which
replicates like anything else.

Now write a note on one (**cmd cmd**, `notes`, **new note**) and watch it
appear in the other's list. Subscribe to a feed on one and the subscription is
on the other; mark an article read on one and it is read on the other. The
article itself is not carried: each device fetches it from the feed.

Press **forget** on either panel and the pair is undone on both, as soon as
that op lands.

Leave the laptop's panel open until the phone has paired: the ticket carries
sixteen random bytes that exist only while that panel is up, and closing it
makes the string worthless. That is what lets it be pasted into a chat at all.

Two devices on one Wi-Fi find each other on the local network and never leave
it. Off it, they meet through a public relay — which forwards live traffic and
stores nothing, so two devices that are never awake at the same time do not
converge until they are. Nothing is lost when they are not: the ops wait in
their origin's log.

## What travels

A subscription, a read mark, a note, the name of a device, and the roster
itself. Not mail, not chats, not calendar events, not article bodies — every
device asks the provider for those itself, and the provider already carries the
read flags that matter. Not the layout either: a phone and a desktop do not
want the same arrangement of the same work.

The table in [the chapter](./book/src/device-sync.md#what-replicates) is the
whole list, and an app adds to it by declaring a table, a key, and the columns
that carry a decision.
