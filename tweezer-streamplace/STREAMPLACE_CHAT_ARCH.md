# Streamplace Chat Architecture

This document describes Streamplace's chat system, its features, and how data flows end-to-end. It is intended to help you run Streamplace chat without requiring the full Streamplace video infrastructure.

## Overview

Streamplace chat is built on the [AT Protocol](https://atproto.com) (the same protocol that powers Bluesky). Chat messages are AT Protocol records stored in users' Personal Data Servers (PDS). A Streamplace backend node indexes these records from the AT Protocol firehose, hydrates them with profile data, and pushes them to clients over WebSockets.

This means chat is inherently federated: any AT Protocol user can send a chat message to any streamer's channel, and any node can index and serve those messages.

---

## Core Features

### Messaging
- **Plain text messages**: Up to 300 graphemes, stored as `place.stream.chat.message` records.
- **Rich text facets**: Mentions (`@handle`) and URLs are parsed into AT Protocol facets and rendered as clickable links.
- **Replies**: Messages can reply to other messages. The reply reference includes both `root` and `parent` strong refs (same threading model as Bluesky posts).
- **Local optimistic updates**: Messages appear in the UI immediately upon send, then are reconciled with the server-confirmed record.

### User Identity & Customization
- **AT Protocol identity**: Users authenticate with their PDS via OAuth. Handles and DIDs come from the AT Protocol ecosystem.
- **Chat colors**: Users can set a custom RGB color for their name via `place.stream.chat.profile`. If unset, a deterministic color is derived from the user's DID.
- **Self-labels**: Users can self-label (e.g., `bot`) in their chat profile.
- **Badges**: Up to 3 badges per message. The first slot is server-controlled (mod, streamer, bot). The remaining slots are user-selected from earned badges (VIP, event). See [Badges](#badges) below.
- **Custom emotes**: Streamers and users can create and share custom emote packs. See [Emotes & Emoji](#emotes--emoji) below.

### Input & UX
- **Mention autocomplete**: Typing `@` suggests handles from recent chat participants.
- **Emoji autocomplete**: Typing `:` triggers emoji search by alias, name, or keyword.
- **Slash commands**: Extensible slash command system (e.g., teleport commands).
- **Reply gestures**: Swipe-to-reply on mobile, hover actions on desktop.
- **Scroll management**: Auto-scroll to bottom, scroll-to-bottom button when scrolled up, message hide-after timeout support.
- **Chat popout**: Web-only feature to open chat in a separate popup window.

### Moderation
- **Hide message (gate)**: Moderators can hide individual messages by creating a `place.stream.chat.gate` record referencing the message URI. Hidden messages are filtered from chat history and removed from live clients.
- **Block user (ban)**: Moderators can block users by creating an `app.bsky.graph.block` record in the streamer's repo. All messages from blocked users are removed from chat.
- **Delete own message**: Authors can delete their own messages via `com.atproto.repo.deleteRecord`.
- **Report messages**: Users can report messages via `com.atproto.moderation.createReport`.
- **Pin messages**: Moderators can pin a message for prominent display. Pins can have an expiration time or last until stream end. Only one pin is active at a time.
- **Delegated moderation**: Streamers can grant moderation permissions (`ban`, `hide`, `message.pin`, `livestream.manage`) to other users via `place.stream.moderation.permission` records.

### System Messages
The backend injects system messages for:
- Stream start/end
- Teleport arrivals (with viewer counts and source chat profiles)
- Command errors
- General notifications

System messages use a synthetic author DID `did:sys:system`.

### Badges
Badges are small icons displayed next to usernames in chat. There are two categories:

**Server-controlled badges** (always first slot):
- **Streamer**: The user is the streamer.
- **Mod**: The user has delegated moderation permissions for this streamer.
- **Bot**: The user self-labels as `bot` in their `place.stream.chat.profile`.

**Issuance-based badges** (user-selected, up to 2 additional slots):
- **VIP**: Streamer-issued badges that only appear in that streamer's chat. Created via `place.stream.badge.def` + `place.stream.badge.issuance`.
- **Event**: Global badges issued by authorized global issuers. Appear in any chat.

A user selects which earned badges to display in their `place.stream.chat.profile` (`badges.streamer` and `badges.global`). When the backend hydrates a message, it resolves the issuance records, verifies the recipient matches, checks the definition still exists, and generates `badgeView` objects. Badge images are served via the Bluesky CDN (`cdn.bsky.app`).

### Emotes & Emoji
Streamplace supports two kinds of emoji:

**Standard Unicode emoji** (client-side only):
- The frontend bundles a stripped-down `emoji-data-stripped.json` generated from `emojibase-data`.
- Typing `:` triggers fuzzy search across aliases, names, keywords, and IDs.
- No backend involvement. Skin tone support is client-side.

**Custom emotes** (AT Protocol records):
- **`place.stream.emote.pack`**: A named collection of emotes. The `openInMyChat` flag allows followers of the pack author to use those emotes in the author's chat.
- **`place.stream.emote.item`**: An individual emote image (PNG, GIF, WebP, AVIF, or JXL, up to 512KB), with a short name, alt text, optional creator DID, and a reference to its pack.
- **`place.stream.emote.packDelegation`**: Grants a specific user global permission to use emotes from a pack. Can be scoped to specific emote URIs.
- **Firehose indexing**: The backend indexes packs, items, and delegations into SQL.
- **Resolution**: `GET /xrpc/place.stream.emote.getEmotePacks` returns available packs for the authenticated user (streamer packs if following + delegated packs). `getEmotePack` returns a single pack with hydrated emote views.
- **Usage in chat**: Custom emotes are referenced via richtext facets (`place.stream.richtext.facet#emote`) using `:name:` syntax. The frontend renders them inline as images. Hover/press on an emote opens an `EmojiCard` showing the pack owner and creator.
- **Images served via**: `https://cdn.bsky.app/img/feed_fullsize/plain/{did}/{cid}@webp`

### Livestream Context
Chat is tightly coupled to a livestream context in the full app:
- Viewer counts are pushed every 3 seconds over the same WebSocket.
- Livestream metadata (title, status) arrives over the same socket.
- Teleports (viewer redirection between streams) are announced in chat.

---

## Data Models (Lexicons)

### `place.stream.chat.message`
The core chat message record.
```json
{
  "$type": "place.stream.chat.message",
  "text": "hello world",
  "createdAt": "2024-01-01T00:00:00Z",
  "streamer": "did:plc:...",
  "facets": [...],
  "reply": {
    "root": {"uri": "...", "cid": "..."},
    "parent": {"uri": "...", "cid": "..."}
  }
}
```

### `place.stream.chat.profile`
Per-user chat customization.
```json
{
  "$type": "place.stream.chat.profile",
  "color": {"red": 255, "green": 100, "blue": 50},
  "selfLabels": ["bot"],
  "badges": {
    "streamer": [{"streamer": "did:plc:...", "badge": {"uri": "...", "cid": "..."}}],
    "global": {"uri": "...", "cid": "..."}
  }
}
```

### `place.stream.chat.gate`
Moderation record to hide a message.
```json
{
  "$type": "place.stream.chat.gate",
  "hiddenMessage": "at://did:plc:.../place.stream.chat.message/..."
}
```

### `place.stream.chat.pinnedRecord`
Pin a message for display.
```json
{
  "$type": "place.stream.chat.pinnedRecord",
  "pinnedMessage": "at://did:plc:.../place.stream.chat.message/...",
  "pinnedBy": "did:plc:...",
  "createdAt": "2024-01-01T00:00:00Z",
  "expiresAt": "2024-01-01T01:00:00Z"
}
```

### `place.stream.moderation.permission`
Delegate moderation powers.
```json
{
  "$type": "place.stream.moderation.permission",
  "moderator": "did:plc:...",
  "permissions": ["ban", "hide", "message.pin"],
  "createdAt": "2024-01-01T00:00:00Z",
  "expirationTime": "2024-12-01T00:00:00Z"
}
```

### `place.stream.badge.def`
Defines a custom badge's appearance and type (`vip` or `event`).
```json
{
  "$type": "place.stream.badge.def",
  "name": "Streamer VIP",
  "description": "Very important person",
  "badgeType": "place.stream.badge.defs#vip",
  "image": {"$type": "blob", "ref": {...}, "mimeType": "image/png", "size": 12345},
  "createdAt": "2024-01-01T00:00:00Z"
}
```

### `place.stream.badge.issuance`
Grants a badge to a specific recipient.
```json
{
  "$type": "place.stream.badge.issuance",
  "did": "did:plc:...",
  "badge": {"uri": "at://.../place.stream.badge.def/...", "cid": "..."},
  "createdAt": "2024-01-01T00:00:00Z"
}
```

### `place.stream.emote.pack`
A named collection of custom emotes.
```json
{
  "$type": "place.stream.emote.pack",
  "name": "My Emotes",
  "description": "Cool custom emotes",
  "openInMyChat": true,
  "createdAt": "2024-01-01T00:00:00Z"
}
```

### `place.stream.emote.item`
A single custom emote image.
```json
{
  "$type": "place.stream.emote.item",
  "name": "dan",
  "image": {"$type": "blob", "ref": {...}, "mimeType": "image/webp", "size": 45000},
  "alt": "dan emote",
  "pack": "at://did:plc:.../place.stream.emote.pack/...",
  "createdAt": "2024-01-01T00:00:00Z"
}
```

### `place.stream.emote.packDelegation`
Grants a user permission to use emotes from a pack.
```json
{
  "$type": "place.stream.emote.packDelegation",
  "did": "did:plc:...",
  "pack": {"uri": "at://.../place.stream.emote.pack/...", "cid": "..."},
  "emotes": [{"uri": "...", "cid": "..."}],
  "createdAt": "2024-01-01T00:00:00Z"
}
```

---

## Data Flow: How Chat Works End-to-End

### 1. Sending a Message
1. User types in `ChatBox` and hits send.
2. Frontend runs `RichText.detectFacets()` to find mentions and links.
3. Frontend creates a `place.stream.chat.message` record with `streamer` set to the target streamer's DID.
4. Record is written to the sender's PDS via `com.atproto.repo.createRecord`.
5. A local optimistic message is inserted into the chat store immediately.

### 2. Indexing (Backend)
1. The Streamplace backend subscribes to the AT Protocol relay firehose (`com.atproto.sync.subscribeRepos`).
2. When a commit contains a `place.stream.chat.message`, the `ATProtoSynchronizer` processes it.
3. It checks if the sender is blocked by the streamer (`app.bsky.graph.block`). If so, the message is dropped.
4. It validates links (rejects `javascript:` URLs), truncates text to 300 graphemes, and saves the message to the local SQL database (`ChatMessage` model).
5. It hydrates the message:
   - Author profile (handle, DID)
   - Chat profile (color, badges)
   - Reply parent (if applicable)
   - Mod badges (if the sender is a moderator or the streamer)
6. The hydrated `place.stream.chat.defs#messageView` is published to the in-memory `Bus` under the streamer's DID.

### 3. Distribution (Backend)
1. The `Bus` is a pub/sub system keyed by streamer DID.
2. All WebSocket connections for that streamer receive the message.
3. The WebSocket endpoint (`/api/websocket/:repoDID`) also sends:
   - Viewer counts every 3 seconds
   - Livestream metadata
   - Pinned messages
   - Teleport arrivals
   - Segment/rendition info (video-specific)

### 4. Receiving (Frontend)
1. The frontend opens a WebSocket to `/api/websocket/:streamerDID`.
2. Incoming messages are batched (250ms) to reduce React re-renders.
3. `handleWebSocketMessages` routes each message by `$type`:
   - `place.stream.chat.defs#messageView` -> added to chat index
   - `place.stream.chat.gate` -> message is hidden
   - `place.stream.defs#blockView` -> all messages from blocked DID are removed
   - `place.stream.chat.defs#pinnedRecordView` -> displayed as pinned comment
4. `reduceChatIncremental` maintains a sorted chat list, handles reply hydration, replaces optimistic local messages with confirmed ones, and deduplicates.

### 5. Deletion
- Author deletes: `com.atproto.repo.deleteRecord` on the sender's PDS. The firehose emits a tombstone, backend marks `deleted_at`, and pushes a `deleted: true` message view to clients.
- Labeler deletion: If a moderation labeler labels a chat message, the backend looks it up, marks it deleted, and pushes the deletion to clients.
- Gate: A moderator creates a `place.stream.chat.gate` in the streamer's repo. The backend indexes it and publishes the gate to the bus, causing clients to remove the message.

---

## Backend Architecture

### Key Components
| Component | Purpose |
|-----------|---------|
| `ATProtoSynchronizer` | Subscribes to AT Protocol firehose, indexes records into SQL |
| `Bus` | In-memory pub/sub for real-time message distribution |
| `Model` | SQL database layer (GORM). Stores chat messages, profiles, gates, blocks, pins |
| `API` | HTTP/WebSocket server. Serves chat history and live updates |
| `spxrpc` | XRPC handlers for moderation actions (`place.stream.moderation.*`) |

### Database Schema (Simplified)
- `chat_messages`: Stores message CBOR, URI, CID, repo DID, streamer DID, reply CID, deleted_at
- `chat_profiles`: Stores user's chat profile CBOR, indexed by repo DID
- `gates`: Hidden message references, indexed by streamer repo DID
- `blocks`: AT Protocol blocks (`app.bsky.graph.block`), indexed by streamer repo DID
- `pinned_records`: Pin records with expiration support
- `labels`: Moderation labels from subscribed labelers
- `repos`: Cached AT Protocol repo metadata (handle, DID)
- `badge_defs`: Badge definitions (name, type, image CID, description)
- `badge_issuances`: Badge grants (recipient DID, badge def URI, issuer DID)
- `emote_packs`: Emote pack metadata (name, open_in_my_chat, repo DID)
- `emote_items`: Individual emotes (name, image CID, pack URI, alt, creator DID)
- `emote_pack_delegations`: Delegation grants (recipient DID, pack URI, allowed emotes)

### Moderation API (XRPC)
These endpoints allow delegated moderation (i.e., a moderator acting on behalf of a streamer):

| Endpoint | Action |
|----------|--------|
| `POST /xrpc/place.stream.moderation.createBlock` | Ban a user |
| `POST /xrpc/place.stream.moderation.deleteBlock` | Unban a user |
| `POST /xrpc/place.stream.moderation.createGate` | Hide a message |
| `POST /xrpc/place.stream.moderation.deleteGate` | Unhide a message |
| `POST /xrpc/place.stream.moderation.createPin` | Pin a message |
| `POST /xrpc/place.stream.moderation.deletePin` | Unpin a message |
| `POST /xrpc/place.stream.moderation.updateLivestream` | Update stream title |

**Emote API:**

| Endpoint | Action |
|----------|--------|
| `GET /xrpc/place.stream.emote.getEmotePacks` | List emote packs available to the viewer for a streamer |
| `GET /xrpc/place.stream.emote.getEmotePack` | Get a single emote pack with hydrated items |

All delegated moderation endpoints validate OAuth, check `place.stream.moderation.permission` records, and audit-log the action.

---

## Frontend Architecture

### Key Hooks & Stores
| Hook/Store | Purpose |
|------------|---------|
| `useChat()` | Returns the current sorted chat message array |
| `useCreateChatMessage()` | Sends a message to the user's PDS |
| `useDeleteChatMessage()` | Deletes the user's own message |
| `usePinChatMessage()` | Creates a pin (streamer or delegated) |
| `useUnpinChatMessage()` | Removes a pin |
| `useSubmitReport()` | Reports a message via AT Protocol |
| `useCanModerate()` | Checks if current user has mod permissions |
| `useLivestreamStore()` | Zustand store holding chat, authors, pinned comment, viewer count |

### Key Components
| Component | Purpose |
|-----------|---------|
| `Chat` | Message list (FlatList), scroll handling, system message rendering |
| `ChatBox` | Input with mention/emoji autocomplete, slash commands, reply UI |
| `RenderChatMessage` | Individual message rendering with facets, timestamps, badges |
| `ModView` | Moderation dropdown (hide, ban, pin, delete, report) |
| `UserProfileCard` | Hover/press user card showing profile details |
| `BadgeDisplayRow` | Renders mod/streamer/bot/VIP badges next to usernames |
| `EmojiSuggestions` | Colon-triggered emoji autocomplete (standard + custom) |
| `EmojiCard` | Hover/press overlay showing emote pack owner and creator |

---

## Running Chat Without Streamplace Video

If you want to run chat independently of Streamplace's video infrastructure, you need to provide the following:

### Required Backend
1. **AT Protocol Firehose Consumer**
   - Subscribe to `com.atproto.sync.subscribeRepos` from a relay.
   - Filter for chat records: `place.stream.chat.message`, `place.stream.chat.gate`, `place.stream.chat.pinnedRecord`, `place.stream.chat.profile`, `app.bsky.graph.block`, `place.stream.moderation.permission`.
   - Filter for badge records: `place.stream.badge.def`, `place.stream.badge.issuance`.
   - Filter for emote records: `place.stream.emote.pack`, `place.stream.emote.item`, `place.stream.emote.packDelegation`.
   - Store and index these records in a database.

2. **Database**
   - Store chat messages with fields: URI, CID, author DID, streamer DID, message CBOR, created_at, deleted_at, reply_to_cid.
   - Store chat profiles, gates, blocks, pins.
   - Store badge definitions and issuances.
   - Store emote packs, emote items, and pack delegations.
   - Support querying recent messages by streamer DID with block/gate/label filters.

3. **WebSocket API**
   - Endpoint: `GET /api/websocket/:streamerDID`
   - On connect, send the last N messages (hydrated), active pin, and streamer profile.
   - Subscribe to the bus for that streamer DID and push new messages, gates, blocks, pins, and viewer counts.
   - Send periodic ping messages to keep connections alive.

4. **REST API**
   - `GET /api/websocket/:streamerDID` (WebSocket upgrade) for live updates.
   - Optional: `GET /xrpc/place.stream.moderation.*` if you want delegated moderation.
   - Optional: `GET /xrpc/place.stream.emote.getEmotePacks` and `getEmotePack` if you want custom emotes.

5. **PDS Client / OAuth**
   - To send messages, users need an AT Protocol PDS account and OAuth flow.
   - The backend needs to validate OAuth sessions for moderation endpoints.

### Required Frontend
1. **AT Protocol Agent**
   - Authenticate users via OAuth against their PDS.
   - Use `@atproto/api` to create/delete records.

2. **Chat Store**
   - Zustand or equivalent store holding `chatIndex`, `chat`, `authors`, `pinnedComment`.
   - `reduceChatIncremental` logic to merge new messages, handle deletions, blocks, reply hydration.

3. **WebSocket Connection**
   - Connect to your backend's `/api/websocket/:streamerDID`.
   - Batch incoming messages and route by `$type`.

4. **UI Components**
   - `Chat`, `ChatBox`, `RenderChatMessage`, `ModView`, `BadgeDisplayRow`.
   - RichText facet rendering for links, mentions, and custom emotes.
   - `EmojiSuggestions` for colon-triggered autocomplete.
   - `EmojiCard` for emote metadata on hover/press.
   - Bundle `emoji-data-stripped.json` for standard unicode emoji search.

### What You Can Drop
- Video segment ingestion, RTMP, WebRTC playback.
- `place.stream.livestream` records (unless you want stream status).
- `place.stream.segment` records.
- Livepeer gateway integration.
- Thumbnail generation.
- Teleport logic (unless you want cross-stream raids).

### Minimal Independent Chat Stack
```
User PDS (Bluesky or self-hosted)
      |
      v
AT Protocol Relay Firehose
      |
      v
Your Chat Backend (Go/Node/Rust)
  - Firehose consumer (chat, badges, emotes)
  - SQL database
  - Bus / pub-sub
  - WebSocket server
  - Optional: Emote pack resolver API
      |
      v
Your Chat Frontend (React/React Native)
  - AT Protocol OAuth login
  - WebSocket client
  - Chat UI components
  - Emoji data bundle + emote image renderer
```

---

## Security Considerations

- **Message validation**: Reject `javascript:` URLs. Truncate text to 300 graphemes.
- **Block enforcement**: Check `app.bsky.graph.block` before indexing messages. Also filter blocks in history queries.
- **Gate enforcement**: Check `place.stream.chat.gate` in history queries and live distribution.
- **Label enforcement**: If you subscribe to moderation labelers, filter labeled messages.
- **Permission validation**: For delegated moderation, always verify the moderator's `place.stream.moderation.permission` record in the streamer's repo.
- **Rate limiting**: The original backend rate-limits WebSocket connections by IP.

---

## Summary

Streamplace chat is a federated, AT Protocol-based chat system where:
- **Messages live in user repos** as standard AT Protocol records.
- **Any node can index and serve chat** by consuming the firehose.
- **Moderation is record-based** (gates, blocks, permission delegations).
- **Badges and emotes are also AT Protocol records** — fully federated and indexable by any node.
- **Real-time delivery** happens over WebSockets backed by an in-memory bus.

To run chat independently, you primarily need: a firehose consumer, a database, a WebSocket server, and an AT Protocol-aware frontend.
