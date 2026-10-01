# Using the groups backend

`crates/nip29` is the backend for [NIP-29](https://github.com/nostr-protocol/nips/blob/master/29.md)
relay-based groups. It is a GPUI crate: it owns one registry entity and one entity
per group, subscribes to the relays, caches everything locally and hands the UI
`chat::Message`s. It owns no strings a user reads — the UI layer decides every
rendering. This document is the map from a UI action to the calls it makes.

A group is a `(relay, id)` pair. The relay enforces membership and signs the
group's state; there is no cross-relay identity, no encryption, and no portable
"create". The same `id` on two relays is two groups (a fork), which the backend
models as two entities.

## Modules

| File | Owns |
| --- | --- |
| `src/protocol.rs` | Pure NIP-29 value types and their `impl` blocks: `GroupId`, `GroupKey`, `GroupReference`, `GroupMetadata`, `GroupAdmins`/`GroupMembers`/`GroupRoles`/`GroupParticipants`, `Pin`/`GroupPins`, `LivekitSession`, `GroupList`/`GroupListEntry`, `Membership`, `TimelineWindow`, plus the filter and event builders on `GroupId`. No I/O. |
| `src/group.rs` | The `Group` entity: one group's state, timeline, membership, writes, the envelope cache helpers and the two LiveKit calls. `GroupEvent`. |
| `src/lib.rs` | The `GroupsRegistry` global, `init`, the notification pump, the account's `10009` list, the fork watch and relay browsing. `GroupsEvent`. |

`protocol` is re-exported at the crate root (`pub use protocol::*`), so callers
write `nip29::GroupKey`, `nip29::GroupMetadata`. `group` is re-exported the same
way (`Group`, `GroupEvent`).

The crate's constants are the protocol's: `RENDER_KINDS` (`1`, `9`, `11`, `1111`,
`7`), `STATE_KINDS` (`39000`–`39004` and `39005`), `MEMBERSHIP_KINDS` (`9000`,
`9001`), `PIN_LIST` (`39005`), `UPDATE_PIN_LIST` (`9010`), `TIMELINE_WINDOW`
(50) and the `previous`-tag shape (3 refs, 8 hex chars).

## Turning it on

One line, after the other registries and gated on nothing — the registry does its
own sign-in check:

```rust
nip29::init(cx);
```

It builds the global entity, subscribes to `NostrRegistry`, and on the first
sign-in (or a signer change) resets its state, restarts the pump and loads the
account's `10009`. No groups exist before a sign-in, and everything is torn down
when the signer changes, so the crate never leaks one account's groups into
another.

`nip29::init(cx)` is already called in `desktop/src/main.rs` and `web/src/lib.rs`.

```rust
use nip29::GroupsRegistry;

let groups = GroupsRegistry::global(cx);   // Entity<GroupsRegistry>
groups.read(cx).groups();                  // &[Entity<Group>]
groups.read(cx).group(&key, cx);           // Option<Entity<Group>>
```

`GroupsEvent` is emitted on the registry; read it back on the event. The sidebar
subscribes to `GroupsEvent` and renders `groups()` — pending and refused groups
are in that list alongside members, and their `Group::membership()` says which.

```rust
#[derive(Debug, Clone)]
pub enum GroupsEvent {
    Updated,
    Error(String),
    Browsed(Box<GroupCandidate>),
    BrowseSettled(RelayUrl),
}
```

`Updated` fires on any change to `groups()`; `Error` is only emitted when
publishing a `9007` (create) is refused, and carries the relay's message.
`Browsed` and `BrowseSettled` carry a browse: one group the relay announced, and
the end of the answer (see [Browsing a relay](#browsing-a-relay)). They are
fan-out, not stored — whatever opened the browse owns the candidates.

## Value types

```rust
use nip29::{GroupId, GroupKey, GroupReference, GroupMetadata};

let id = GroupId::new("pizza-lovers")?;             // trimmed, non-empty
let key = GroupKey::new(relay_url.clone(), id);     // the (relay, id) pair
```

A `GroupKey` is the identity of a group *as this backend sees it*: everything
that addresses a group — `group`, `join_key`, `create`, `load_more`, `leave`,
`browse` results — takes one. `Group::key()` returns the one an entity was built
with, and `GroupReference::key()` returns the one a link resolves to.

A reference is a link, and only a link:

```rust
let reference = GroupReference::parse("naddr1…?invite=CODE")?;   // or .parse::<GroupReference>()
```

`GroupReference::parse` (and `FromStr`) accepts only a kind-`39000` `naddr` with a
relay hint. The `?invite=` suffix the NIP defines is split off before bech32
decoding and lives on `reference.invite()`; a missing relay hint is an error,
because there is nowhere to send the join request. `id@relay` is deliberately not
accepted.

`GroupMetadata` is both the read model and the `9002`/`9007` payload, and it is
`Default`:

```rust
let metadata = GroupMetadata {
    name: Some("Pizza Lovers".into()),
    private: true,
    ..Default::default()
};
```

`parse(event)` fills it from a relay-signed `39000` (and keeps the event's id and
author privately, which is what `naddr` is built from). `name()`, `supports(kind)`
and the public flag fields (`private`, `restricted`, `hidden`, `closed`,
`livekit`, `parent`, `children`, `supported_kinds`) are what a group header reads.
`supports` distinguishes "tag absent" (all kinds) from "tag present with no
values" (no kinds) — that is the difference between a normal group and an AV-only
one.

## The account's group list

`10009` ("simple groups") is the member's own list of memberships, synced across
devices. The registry keeps it in `list: GroupList` and owns its whole lifecycle:
it is loaded at sign-in, `upsert`ed on join/create, `remove`d on leave, and
re-published to `to_nip65()` after every change. A caller never touches it.

```rust
// `10009` tag shape: ["group", id, relay, name?] plus ["r", relay]
```

Loading is a subscription plus a database read, the same read model as everything
else: subscribe to our own `10009` under `nip29-list`, then read the newest from
`client.database()` and merge it. A newer list arriving from another device is
merged union-only — the backend never diffs, so a removal another client made is
not detected, and the local state is never walked backwards. `GroupList::parse`
requires `["group", id, relay]` and takes the third value as the display name.

## Joining

```rust
GroupsRegistry::global(cx).update(cx, |registry, cx| {
    registry.join(reference, cx);          // or join_key(key, invite, cx)
});
```

`join` is `join_key(reference.key(), reference.invite().to_owned())`, and
`join_key` is what every other path calls too, because a browse result has a key
and no `naddr`. The sequence is:

1. `ensure` the group: create the entity, register it in `groups()`, subscribe to
   its relay (the standing `grp-<hash>` REQ, below) and reload it from the local
   cache.
2. Add the entry to `10009` optimistically and publish the list, so the group is
   listed the moment the user asks for it.
3. Publish `key.id().join_request(invite, "")` — the `h` tag plus, when the link
   had one, a `code` tag. `Group::mark_pending(now)` runs immediately, so
   `Group::membership()` is `Pending { since }` while the relay decides.

The relay either emits `9000` (the standing subscription folds it into
`Membership::Member`), or answers the `9021` with `OK false` — the registry
catches that and sets `Membership::Refused { reason }` from the relay's message.
The NIP promises that message explains whether the rejection is final, pending
review or payment-required, so show it verbatim and offer a retry. A join to a
`closed` group is refused the same way, unless the invite code is honoured.

Join needs no `39000` first: nothing is fetched, the `9021` is the whole request.
A hidden group needs no special handling either — the state simply never arrives.

## Reading a group

`Group` is an entity with no protocol state in the view. A panel reads it and
subscribes to `GroupEvent`:

```rust
use nip29::{Group, GroupEvent};

let Some(group) = registry.read(cx).group(&key, cx) else {
    return;
};
let rows = group.read(cx).rows();                    // &[nip29::Row]
```

```rust
cx.subscribe(&group, |this, group, event, cx| match event {
    GroupEvent::Incoming(..) | GroupEvent::Updated => cx.notify(),
    GroupEvent::Refused(reason) => this.show_error(reason, cx),
    GroupEvent::Eose => this.stop_spinner(cx),
    GroupEvent::Sent(_) => {}
})
```

`GroupEvent` is the full surface:

| Variant | Means |
| --- | --- |
| `Incoming(Event)` | a fresh renderable row was folded in (use it for system notifications when the window is inactive) |
| `Sent(Event)` | our own event was accepted by the relay; the registry re-reads the group, the panel normally ignores it |
| `Updated` | metadata, state, membership, pins or a finished page changed |
| `Eose` | the standing subscription settled |
| `Refused(String)` | the relay `CLOSED` the standing subscription, with its message |

The reads are all accessors: `key`, `metadata`, `members`, `admins`, `roles`,
`participants`, `pins`, `membership`, `elsewhere`, `rows`, `loading`,
`loading_more`, `oldest`, `refused`. `display_name` reads the group's name and
falls back to its id; `display_image` is the picture, for a row or avatar.

`rows` is the whole timeline in `created_at` order, one entry per renderable
event. `Row::Message(Box<chat::Message>)` is a chat row; `Row::Activity(Activity)`
is a moderation row — who joined (`9021`), who left (`9022`), who was removed
(`9001`) and who was given roles (`9000` with roles; a `9000` without roles is the
relay echoing a join we already show and is not a row). `Activity::subject()` is
the affected person (the `p` tag target, or the author when there is none) and
`Activity::roles` the assigned roles, so the view only chooses wording and an
icon. `Group::message(id)` looks a chat row up for a pin label.

### The read model

There is **no `fetch_events` anywhere** in the crate. A group is read by
subscribing, then querying the local database — the same model as `crates/chat`
and `crates/community`:

```
subscribe grp-<hash>            the global pump observes every REQ on the client
      │  (timeline + membership + one state filter per kind, one relay)
      ▼
save_envelope                  30078, d = event id, r = "<relay>|<id>", k = kind
      │                         content = the raw event JSON, signed by crate-local keys
      ▼
load_envelopes                 query by the r tag, parse, sort by created_at
      │
      ▼
Group::hydrate                 the only place entity state changes
```

The standing REQ carries the timeline (`GroupId::timeline_filter(100)`), our own
membership (`membership_filter(me)`) and `state_filters()` — **one filter per
state kind**, because a relay that serves group metadata is allowed to refuse a
filter that mixes metadata kinds with other kinds (`groups.hzrd149.com` answers
`blocked: it's not allowed to mix metadata kinds with others` and drops the whole
REQ, which is what hid a group's name and picture until the filters were split).
Its id is `grp-` plus 8 hex of a hash of the
key — NIP-01 caps ids at 64 characters, and the pump needs a prefix to recognise
its own subscriptions, so the real key is held in the registry's `routes`, not
encoded in the id.

The envelope cache is what makes the timeline fork-safe and restart-safe without
teaching the nostr database about relays (a stored event carries no origin): the
`r` tag *is* the relay scoping. It is also why every event is re-read instead of
consumed off the wire — the pipeline dedups, sorts and survives a restart, and a
page, a live tail and a local send all take the same path.

`hydrate` partitions an envelope run:

- `STATE_KINDS` → `metadata`, `admins`, `members`, `roles`, `participants`,
  `pins` (relay-signed, read-only, addressable by `d`).
- `MEMBERSHIP_KINDS` → our membership only (see below).
- `RENDER_KINDS` → a `Row::Message`, inserted in `created_at` order.
- `ACTIVITY_KINDS` → a `Row::Activity`, inserted in `created_at` order. A `9001`
  that answers a `9022` from the same subject within five minutes is dropped, so a
  plain leave reads once.
- everything else is kept only for the `previous` window.

Every `h`-tagged event feeds the window regardless of kind. The window is the
NIP's timeline reference: the last 50 events seen on that relay, excluding our
own, first 8 hex characters of their ids, up to 3 per send. The write path
attaches it automatically via `Group::previous_tag(me)`, so callers never build
`previous` themselves.

`Group::loading()` is true until EOSE; `Group::refused()` carries the relay's
`CLOSED` message when it never settles. A relay may answer a restricted or
private group with `restricted:` / `auth-required:` — the state stays, the group
just reads empty, and that is not an error toast.

### Older messages

```rust
GroupsRegistry::global(cx).update(cx, |registry, cx| {
    registry.load_more(&key, cx);
});
```

It is a no-op while a page is in flight (`Group::loading_more()`) or before
anything is cached. Otherwise it sends a `pg-<hash>` REQ (`timeline_page(50,
until)`) alongside the standing one — paging must not replace the live tail — and
the pump caches each page event and reloads the group exactly like a live event.
The cursor is `Group::oldest()`, the oldest `h`-tagged event ever hydrated; `until`
is inclusive, so a page repeats its first event, which the dedup absorbs for free.
EOSE, a page `CLOSED` or the 10s auto-close all clear the spinner.

One relay behaviour is worth a spinner that can be cancelled: a relay that accepts
the REQ but never EOSEs leaves `loading_more` true, because the auto-close closes
the subscription locally with no notification at all.

## Sending

Every write goes through one path — `Group::dispatch` builds the event with
`key.id()` and the current `previous` tag, `Group::publish` adds and connects the
relay, signs with the app signer and sends to that one relay:

```rust
let sent = group.update(cx, |group, cx| group.send_message(&text, replies, cx));
cx.spawn(async move |_, _| {
    if let Err(error) = sent.await {
        // the relay's OK-false message, verbatim: "invalid: ...", "restricted: ...", "duplicate: ..."
    }
});
```

| Call | Wire |
| --- | --- |
| `send_message(content, replies: Vec<EventId>, cx)` | kind `9` + `h` + `e` per reply |
| `send_reaction(target: EventId, emoji: String, cx)` | kind `7` + `h` + `e` |
| `put_user(public_key, roles, cx)` | `9000` |
| `remove_user(public_key, cx)` | `9001` |
| `edit_metadata(&GroupMetadata, cx)` | `9002` |
| `delete_event(EventId, cx)` | `9005` |
| `create_invite(code, cx)` | `9009` |
| `update_pins(Vec<Pin>, cx)` | `9010` |

Each returns `Task<Result<()>>` and each resolves only once the relay has
answered: `OK true` emits `GroupEvent::Sent` and the registry reloads the event
through the normal cache path, so the timeline shows a send without waiting for
the live tail; `OK false` is an `Err` carrying the relay's message. There is no
offline queue — NIP-29 relays reject late publication, so a send either goes now
or fails visibly.

Kind 9 is the default message kind. When `metadata().supports(Kind::ChatMessage)`
is false — an AV-only group, or one whose `supported_kinds` omits it — disable the
composer instead of sending a kind the relay does not take.

`replies` are plain `EventId`s; they become `e` tags. There is no thread model in
the backend beyond that: `chat::Message::replies_to` already reads them back.

## Membership

Membership is derived from the relay's own events, never guessed:

```rust
pub enum Membership {
    Unknown,
    Pending { since: Timestamp },
    Member,
    Refused { reason: Option<String> },
    Removed,
}
```

`Membership::from_events(&[...], me)` takes the newest `9000`/`9001` whose `p`
tags name us — on an exact timestamp tie it prefers `9001` — and `Unknown` when
there is none, which the NIP says means "not a member". The `Group` keeps only
the membership events that mention us, so `refresh_membership` costs nothing.

`Pending` and `Refused` are the two local states, because the relay does not fold
them into an event: the join path sets them from the `9021` send result. A
`Member` is never overwritten by `mark_pending` or `mark_refused`, so a stale
refusal cannot demote a member.

Count a member who is `Unknown`, `Pending` or `Refused` as "not in the room" for
writes, but keep the row visible: pending and refused groups stay in `groups()`
with their state on the row.

## Moderation

The `9000`–`9010` writes are on `Group` and share `dispatch`, so they are the
same shape as sending a message. The backend does not enforce authority — the
relay owns roles and its own policy — it only surfaces the refusal. Gate the UI
on the caller appearing in `Group::admins()` (each entry is a pubkey plus its role
labels), read the relay's supported roles from `Group::roles()`, and let the relay
reject anything it does not want.

`edit_metadata` takes a `GroupMetadata`, so a form can start from
`Group::metadata().cloned().unwrap_or_default()`, mutate, and send. `children`
must carry every child tag, because a `9002` that drops one is rejected by the
relay; the builder re-emits them from the struct.

`delete_event` sends `9005`, and once the relay accepts it the registry drops the
target's cached envelope, evicts the row and re-reads the group, so the timeline stops
showing it. Remote deletions are not folded in: the standing REQ does not carry `9005`.

## Pins

```rust
use nip29::Pin;

let pins: Vec<Pin> = vec![Pin::Event(event_id), Pin::Address(coordinate)];
group.update(cx, |group, cx| group.update_pins(pins, cx));
```

`9010` carries the whole list, so pinning, unpinning, reordering and clearing are
all one call. The relay mirrors it into `39005`, which is in `STATE_KINDS` — so
`Group::pins()` reflects the accepted list on the next reload.

## Subgroups

`GroupMetadata` carries `parent: Option<GroupId>` and `children: Vec<GroupId>`,
and the metadata builder re-emits both, so a create or edit can declare the link.
There is no tree helper: the tree is scoped to one relay, so assemble it from
`Group::metadata()` across `groups()` where all the children have the same relay.
Membership and admin roles do **not** cascade across the link — each group's
`9000`/`9001` and `39001` are authoritative for its own scope — so never infer
access from a parent.

## LiveKit

Two calls, both off the foreground thread and both `&App`:

```rust
if group.read(cx).metadata().is_some_and(|m| m.livekit) {
    let available = group.read(cx).livekit_supported(cx).await?;   // GET /.well-known/nip29/livekit
    let session = group.read(cx).livekit_session(cx).await?;       // LivekitSession { token, server_url }
}
```

`livekit_supported` is an unauthenticated GET that answers `true` only for `204`.
`livekit_session` signs a NIP-98 `Authorization` header (`27235`, with the `u` and
`method` tags the SDK's verifier demands) and returns the JWT and server URL; the
token is a per-join artifact, returned not stored. Both requests carry an explicit
`RedirectPolicy::NoFollow` and a 10s timeout, because the header is signed for
exactly one URL and GPUI applies no timeout unless asked. The backend stops at the
JWT — no LiveKit client library is involved — so the AV room itself is UI work.

Check `metadata().livekit` before offering the button, and read
`Group::participants()` (kind `39004`, `participant` tags, relay-published) for
who is live. Absent and empty are the same value there: nobody is in the room.

## Forks and migration

A `10009` on the *admins'* relays says where each of them thinks the group is. The
registry keeps one standing `nip29-forks` subscription over
`{authors: every admin of every group, kinds: [10009]}` on `ReqTarget::auto` —
which is why it still works when the group's own relay is down — folds each event
into a per-author list, and recomputes every group's `elsewhere`.

```rust
if let Some(moved) = group.read(cx).elsewhere() {
    // the group may have moved or been forked; offer:
    registry.update(cx, |registry, cx| registry.join_key(moved.clone(), None, cx));
}
```

`elsewhere()` is `Some(key)` when an admin of the group lists the same group id at
a different relay. The data cannot distinguish a move from a fork, hence the
neutral name; nothing is auto-joined, because the NIP asks the client to *offer*.
Evaluation is incremental on each event rather than batched on EOSE, since
`ReqTarget::auto` fans out and an early EOSE would evaluate against one relay's
partial view. A 15-minute background tick re-subscribes regardless, which is the
recovery path for a subscription that died or a client that was offline.

`Monitor` is not adopted: it is only reachable through `ClientBuilder::monitor`,
so nothing can read per-relay status without changing `crates/state`. The fork
check does not need it — the admin pubkeys come from the cached `39001`, so the
check runs with the relay gone, just at the next tick.

## Browsing a relay

```rust
registry.update(cx, |registry, cx| registry.browse(relay_url.clone(), cx));

// then, in a GroupsEvent handler:
match event {
    GroupsEvent::Browsed(candidate) => push(candidate),
    GroupsEvent::BrowseSettled(relay) => settled(relay),
    _ => {}
}
```

A `GroupCandidate` is a `GroupKey` plus its parsed `GroupMetadata`; `display_name`
falls back to the id and `display_image` is the picture, the same pair `Group`
offers. `browse` connects the relay, sends one auto-closing `39000` REQ (limit
500, EOSE or 10s) and returns immediately; the pump parses each relay-signed
metadata event and re-emits it as `Browsed`.

**The registry stores no browse result.** It keeps only which relay each `brw-`
REQ belongs to — the same routing bookkeeping as `routes`, and the reason
`Browsed` can carry a boxed candidate instead of the registry holding a `Vec`:
the only copy is the caller's. A browse is transient by nature, so the surface
that asked for it owns it (the browse panel keeps `relay`, `candidates` and
`loading` in its own state, and drops them with the panel).

`BrowseSettled` is driven by the subscribe task itself, not by `EOSE`/`CLOSED`
notifications: it is sent once `subscribe(..).await` returns, which happens on
EOSE, on a refusal, or when the auto-close timeout fires. So the answer always
ends, even against a relay that never EOSEs. A candidate can still arrive after
the settle — the pump and the subscribe task are different producers — which is
why the relay stays mapped. Join a result with `join_key(candidate.key, None, cx)`.

## Creating a group

```rust
let metadata = GroupMetadata { name: Some(name), ..Default::default() };
registry.update(cx, |registry, cx| registry.create(key, metadata, cx));
```

Creation mirrors join: `ensure` the entity, add it to `10009` and publish optimistically,
then send the `9007`. There is no NIP-11 check to lean on, so "where the relay
supports it" is enforced by the relay refusing the event — on refusal the registry
forgets the group and emits `GroupsEvent::Error(reason)`. On success the relay
republishes `39000`, which the standing subscription picks up like any other state.

## Leaving

```rust
registry.update(cx, |registry, cx| registry.leave(&key, cx));
```

`leave` sends `9022`; once the relay accepts, the registry forgets the group:
unsubscribe both REQs, drop the entity and its observer, remove the `10009` entry,
delete every cached envelope for that group's `r` tag, and publish the shortened
list. A failed leave leaves everything untouched, so the group stays. There is no
separate local removal — a group someone else removed us from stays listed until
we leave it ourselves.

## GPUI integration

`init` builds the global; the registry and its groups are the only entities, and
`protocol.rs` has no GPUI dependency at all.

```rust
pub fn init(cx: &mut App) {
    GroupsRegistry::set_global(cx.new(GroupsRegistry::new), cx);
}

impl GroupsRegistry {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalGroupsRegistry>().0.clone()
    }
}
```

- `GroupsRegistry` holds `groups: Vec<Entity<Group>>`, the `10009` `list`,
  `routes` (subscription id → `GroupKey`), the relay behind each live `brw-` REQ,
  one `Subscription` per group, the fork state, and its tasks. It holds no browse
  result: a browse is fan-out, and the caller keeps the candidates.
- `Group` owns everything the panel draws: metadata, admins/members/roles,
  participants, pins, membership, the `Vec<Row>` timeline, the `previous`
  window, `loading`/`loading_more`/`refused`.
- Views render `Entity<Group>` and re-read on `GroupEvent`; no protocol state
  lives in a view.
- The registry re-broadcasts every `GroupEvent::Updated` as `GroupsEvent::Updated`
  and notifies itself, so a view that watches only the registry (the sidebar,
  which renders `groups()` as rows) stays in sync when a group's metadata,
  membership, pins or fork state change. `GroupEvent::Updated` fires for state and
  membership only, never for an arriving row — that is `GroupEvent::Incoming`,
  and it is why a rebuild of the row list does not happen per row. A view that
  renders the timeline watches the `Group` itself.

### Foreground and background

One background task drains `client.notifications()` and never touches an entity;
it sends signals through an unbounded `flume` channel that one foreground
`cx.spawn` consumes, which is the only place entities change:

```
notifications() ──sig──► flume ──recv──► this.update(cx, …) ──► handle_event → reload → hydrate
```

- `handle_notifications` runs on `cx.background_spawn`; it matches
  `ClientNotification::Message` and routes the message by subscription id.
- The consumer runs on `cx.spawn`; every arm is one `this.update(cx, ..)`.
- The listener and the consumer are replaced (`= None` first) on a signer change,
  because a dropped `Task` cancels; that is what makes the reset clean.
- Both `save_envelope` and `load_envelopes` are `cx.background_spawn` work that
  clones the `Client` in, so no database call ever runs on the foreground thread.
- NIP-46 signing is async, so all signing happens inside the background task.

### Subscriptions and routing

One `Client` and one notification stream are shared with `chat`, so every
subscription the crate owns is identified by a prefix and nothing else:

| Id | Meaning |
| --- | --- |
| `nip29-list` | the account's `10009` |
| `nip29-forks` | the admins' `10009` |
| `grp-<hash>` | a group's standing REQ (timeline + membership + state filters) |
| `pg-<hash>` | a "load more" page |
| `brw-<hash>` | a relay browse |

Inbound events are routed by `subscription_id` from `RelayMessage::Event`, never
by kind. That matters because `crates/chat` treats every kind-1059 event as a
NIP-59 gift wrap — anything a new subscription adds must be recognisable by id, and
the pump drops ids it did not mint. Duplicate event ids are filtered in the pump
before they reach the consumer, so a replay cannot double-hydrate.

## Not wired up yet

- **Administration is built on the client side.** The sidebar's `Groups` tab lists
  `GroupsRegistry::groups()`, opens a `GroupPanel`, and offers the new-group and
  join dialogs; `crates/workspace/src/panels/browse.rs` browses a relay and joins
  from a result. The panel has a read-only details dialog, a message menu with
  reactions and admin pin/delete, a pins strip, a fork banner, and leave. Opening a
  group also fills the sidebar with the group itself — banner (the `39000` `banner`
  tag, falling back to its `picture`), avatar and name, a row of info / edit / invite /
  leave buttons, and the collapsible `Admins`/`Members` lists. Clicking a member opens
  a dropdown menu (view profile, copy npub, and — for admins — remove from group).
  There is no members-and-roles dialog: adding a member is done through the invite
  flow, and roles are read-only. The panel has no header: the group's `about` lives
  in the details dialog, and the composer's leading button shows or hides the
  sidebar's group information. What a given account may actually do is the relay's
  call; the client shows the refusal verbatim.
- **The composer uploads to the configured blossom server.** `GroupPanel`'s
  paperclip uploads one file at a time, unencrypted, and appends its URL to the
  message on send — the same plain-attachment path the chat composer uses when a
  message is already being typed. The server comes from
  `AppSettings::get_file_server`; the upload itself is `state::upload`.
- **Remote deletions are not folded in.** A `9005` from another admin never reaches the
  standing REQ, so only our own accepted deletes evict the row.
- **The LiveKit response shape is a guess.** The NIP names no fields, so
  `LivekitSession::parse` reads `token` (or `jwt`) and `url` (or `server_url`) and
  fails loudly on anything else. Re-check it against the relay actually targeted
  before building the AV UI, or the join button fails silently.
- **A relay that never EOSEs bounds how long "load more" spins.**
  `Group::loading_more` relies on EOSE or CLOSED, and the auto-close timeout
  closes the subscription with no notification at all; bind "load more" to
  `Group::loading_more()` and give the user a way out. A browse is fine by
  contrast: `BrowseSettled` comes from the subscribe task returning, so it
  always lands.
- **Subgroups are parsed, not assembled.** `parent`/`children` are on
  `GroupMetadata` and the writes carry them; there is no tree builder, because
  the tree is scoped to a relay and that is a view concern.
- **`GroupMetadata::naddr` needs a parsed metadata.** It builds from the `id` and
  author the `39000` left on the struct, so a hand-built `Default` value errors
  rather than minting a link that points nowhere. A group whose relay publishes
  no `39000` cannot be referenced or joined at all.
