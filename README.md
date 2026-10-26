# gim-client-sdk

Client runtime of gim and every language binding: storage, networking, sync,
outbox, media and UI-facing services on top of the gim-core protocol crates,
exposed to Swift and Kotlin through UniFFI.

The protocol crates (`gim-common`, `gim-proto`, `gim-identity`, `gim-crypto`,
`gim-base`) and the `gim-test` fixtures live in
[gim-core](https://github.com/gimhq/gim-core) and are pinned by commit in
`Cargo.toml`. To work on both repositories at once, add a local override:

```toml
[patch."https://github.com/gimhq/gim-core.git"]
gim-base = { path = "../gim-core/crates/gim-base" }
gim-common = { path = "../gim-core/crates/gim-common" }
gim-crypto = { path = "../gim-core/crates/gim-crypto" }
gim-identity = { path = "../gim-core/crates/gim-identity" }
gim-proto = { path = "../gim-core/crates/gim-proto" }
gim-test = { path = "../gim-core/testing/gim-test" }
```

| Crate | Role |
|---|---|
| `crates/gim-store-sqlite` | rusqlite + SQLCipher `StateStore` + `CryptoStore` on one database |
| `crates/gim-sdk` | async `Client`: HTTP, WebSocket/long-poll, sync, outbox, media |
| `crates/gim-ui` | chat list and timeline `VectorDiff` streams |
| `bindings/gim-ffi` | the only UniFFI crate; builds `libgim_core.{so,a,dylib}` |
| `bindings/android`, `bindings/apple` | packaging scripts |
| `uniffi-bindgen` | `uniffi-bindgen` binary used by the packaging scripts |
| `apps/gim-cli` | Linux CLI client |
| `testing/gim-integration-testing` | tests against a real gim-hub container (`scripts/integration-test.sh`) |

The FFI API is documented in `docs/API.md`. The library, Swift module
(`GIMCore`) and Kotlin package (`im.g.gimcore`) keep their gim-core names.

## Checks

Builds run in a container:

```sh
mkdir -p /srv/.tmp/cache/cargo
docker run --rm --label ai-agent=true -v "$PWD:$PWD" -w "$PWD" \
  -v /srv/.tmp/cache/cargo:/cache/cargo -e CARGO_HOME=/cache/cargo rust:1 \
  sh -c 'rustup component add rustfmt clippy && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test'
```

## Integration tests

`scripts/integration-test.sh` builds gim-hub from `GIM_HUB_SRC` (default
`/srv/gimhq/im-client/hub`) at `GIM_HUB_REV` (default: the pinned commit,
printed by `scripts/gim-hub-dev.sh rev`; exported with `git archive`, so local changes in the checkout are ignored) as
`ai-agent/gim-hub`, runs it on the `traefik` network, waits for `/healthz`,
prints the hub commit and runs `testing/gim-integration-testing` in a `rust:1`
container against it. The hub is stopped on exit. Extra arguments filter the
tests:

```sh
scripts/integration-test.sh                    # everything
GIM_HUB_BUILD=0 scripts/integration-test.sh link   # reuse the image, one test
```

The tests are `#[ignore]`d and skip without `GIM_HUB_URL`, so `cargo test`
stays offline. Scenarios, several clients in one process:

- `tests/scenario.rs`, gim-ffi clients: identity creation, handle claim and
  sign-in after restart; direct chat (receipts, typing, reply, edit,
  reaction, delete, reconnect, restart); file and image (blob) send and fetch;
  group add and remove; group leave (see below); linking a second device with the SAS, which
  then reads and writes in an existing chat, and revoking it (its credential
  is rejected); restoring from the recovery code.
- `group_leave`: a member leaves with a self-Remove Proposal, the remaining
  member commits it by reference, the leaver ends `Left` and gets no later
  messages. `tests/faults.rs` `group_leave_three_members`: of the two
  remaining members the one with the lowest device id commits at once, the
  other waits its 5-15 s delay and finds it committed (one epoch change).
- `tests/faults.rs`, gim-sdk clients over the hub's HTTP API with faults
  injected at the transport (`src/sdk.rs`): a lost reply to an accepted
  Commit and to an application message (the re-post gets the hub's
  idempotent result); two devices committing from the same epoch (409
  `stale_epoch`, catch up, commit again); an expired event cursor recovered
  through the pending Welcomes and per-group backfill. The hub cannot expire
  a cursor on demand (`GIM_HUB_EVENT_RETENTION_DAYS` is at least 1), so the
  410 is injected at the hub's head cursor; the recovery itself runs against
  the hub.

### Federation suite

`GIM_FEDERATION=1 scripts/integration-test.sh` runs `tests/federation.rs`
against a closed federation started by `scripts/gim-hub-dev.sh fed-start`:
`GIM_FED_HUBS` (2, default, or 3) hubs `hub-a`, `hub-b` (`hub-c`) that allow
each other, plus `hub-x` that nobody allows (its own allowlist lists
`hub-a`), on a network created for the run. Each hub has its own root key in
a named volume, all share a generated libp2p PSK (TCP only, so `hub-x` is
refused by the allowlist, not by the network), and the script waits until the
allowed hubs have bound each other. The tests run one at a time in a
`rust:1` container on that network with the Docker socket mounted (they stop
and start `hub-b`); hub logs are printed on failure and the run's
containers, network, volumes and config are always removed (label
`ai-agent-run=gim-fed-<run>`). Without `GIM_FED_HUB_A` the tests are skipped.

```sh
GIM_FEDERATION=1 scripts/integration-test.sh
GIM_FEDERATION=1 GIM_FED_HUBS=3 scripts/integration-test.sh peer_outage
```

Users on different hubs, real gim-core clients: hosting records published at
onboarding resolve on the peer hub; a cross-hub DM with text both ways and an
image each way; a group owned by `hub-a` with members on both hubs (add from
either hub, messages both ways, leave by Proposal from `hub-b`, remove);
concurrent Commits from both hubs (one 409, catch up, recommit); `hub-b`
down: a post to a `hub-b`-owned group (503) and an invite of a `hub-b` user
(claim `unavailable`) go through once it is back; `hub-x` refused (`hub-a`
logs `Hello refused`, nothing reaches either side); `cross_hub_image_fetch`:
the receiver's hub fetches an image uploaded on the sender's hub from its
origin (hub-v2 federation.md §5.6, gim-hub H5).

### Multi-homing suite

`GIM_MULTIHOMING=1 scripts/integration-test.sh` runs `tests/multihoming.rs`
the same way on three allowlisted hubs (`GIM_FED_HUBS=3`; the tests stop and
start `hub-a`). gim-sdk clients open every hub through a recording transport
(`src/sdk.rs`), so the tests see which hub got each submission, claim and
event. Covered: adding a hub (records on both hubs, a linked device learns
and verifies the URL through the self group, a message arrives through both
hubs and is stored once); failover with the primary stopped (sending through
`hub-b`, receiving from `hub-b` and `hub-c`, no duplicates and the same order
after `hub-a` is back); KeyPackages disjoint per hub and never claimed
twice, also while `hub-a` is down, then the last resort and top-up;
`set_primary_hub`; `remove_hub` (`HubOwnsGroups`, `HomeHub`, `LastHub`,
messaging in a group the removed hub owns); a shared URL of another hub stays
unverified without a sign-in.

`GIM_FEDERATION=1 testing/gim-cli-interop.sh` runs the cross-hub CLI variant
(`cli_to_cli_interop_across_hubs`): profiles on `hub-a` and `hub-b`, naming
each other by user id (handles are per hub; every `<handle>` argument also
takes a user id).

## CLI (gim)

`apps/gim-cli` builds `gim`, a command-line client on the public gim-sdk
API, for scripted interop runs against gim-hub and the mobile apps.

```sh
cargo build -p gim-cli          # -> target/debug/gim
```

### Profiles

Every command needs a profile directory: `--profile DIR` (or `GIM_PROFILE`).
It holds the SQLite store (`gim.sqlite3`), the session (`secrets.json`, mode
0600; refused if group/other bits are set) and `config.json` (server and keep
server URLs). `register` (and `restore`, `link`) creates the profile on
`--server URL` (or `GIM_SERVER`; default the built-in `https://hub.g.im`); later
commands read the URL from the profile. The keep server (encrypted backup of
the privacy settings, reports) is the built-in `https://keep.g.im` unless
`--keep-server URL` (or `GIM_KEEP_SERVER`) names another one or `--no-keep` turns
it off; either is saved to the profile whenever given, and `--keep-server default`
goes back to the built-in one. Everything but `report` works without it. One profile is one device: never use it
from two `gim` processes at once (concurrent syncs or sends corrupt its MLS
state); use one profile per simulated user.

Each command restores the session, syncs once, runs and sends what it queued
before exiting. Only `listen` and `wait-for` keep syncing while they run.

### Commands

`<chat>` is a chat id, a unique chat name, or the handle of a direct-chat peer
(`@bob`); `<msg>` is a message id.

| Command | What it does |
|---|---|
| `register <handle> <display_name>` | Register a new user and device |
| `whoami` | This profile's user, device and server |
| `users search <handle>` | Exact-handle lookup returning 0 or 1 users (substring search is not available yet) |
| `chats list` | All chats, most recent first |
| `dm <handle>` | Open (or create) the direct chat; prints the chat id |
| `group create <name> <handle>...` | Create a group; prints the chat id |
| `group add\|remove <chat> <handle>...` | Add or remove members |
| `group rename <chat> <name>`, `group leave <chat>` | Rename or leave a group |
| `members <chat>` | Members and their devices; lists devices known from chat membership only |
| `send <chat> <text> [--reply <msg>]` | Send text |
| `send-image\|send-file <chat> <path> [--caption T] [--reply <msg>]` | Send media |
| `react <msg> <emoji> [--remove]` | Add or remove a reaction |
| `edit <msg> <text>`, `delete <msg>` | Edit or delete an own message |
| `read <chat>` | Mark a chat read (sends read receipts) |
| `history <chat> [--limit N]` | Latest messages, oldest first |
| `typing <chat> [--stop]` | Typing notification |
| `download <msg> <path>` | Save a message's media |
| `listen [--chat C] [--timeout S] [--until T] [--count N]` | Print events as they arrive |
| `wait-for <chat> ...` | Block until a message matches (see below) |

Typing indicators expire on their own. `typing <chat> --stop` from a fresh
process is a no-op: gim-sdk keeps typing state in memory, so only the process
that started typing can stop it.

`wait-for` waits for the chat (DM peer handle, chat name or id) to appear if
the profile has not joined it yet.

### Output and exit codes

With `--json` every command prints exactly one JSON object on stdout, errors
included (`{"error": {"code", "message"}}`); ids are strings, timestamps are
milliseconds. `listen --json` prints JSON lines instead, one event per line,
flushed per line. The shapes are documented in `apps/gim-cli/src/output.rs`
and in `gim <command> --help`.

| Exit code | Meaning |
|---|---|
| 0 | ok |
| 1 | other error |
| 2 | usage error or invalid input |
| 3 | timeout (`wait-for`, `listen --until/--count`) |
| 4 | not found (profile not registered, unknown user, chat or message) |
| 5 | server, network or transport error |

### Scripting with wait-for

Synchronise scripts with `wait-for` (default `--timeout 30`), not sleeps:

```sh
gim wait-for @alice --text "Leaving Split" --from alice     # a new message
gim wait-for "$chat" --msg "$id" --reaction 👍 --from bob   # a reaction
gim wait-for "$chat" --msg "$id" --edited                   # or --deleted
gim wait-for "$chat" --msg "$id" --read-by bob              # or --delivered-to
gim --json wait-for "$chat" --text hi | jq -r .message.id   # exit 3 on timeout
gim --json listen --chat "$chat" --count 1 --timeout 30     # one JSON line
```

### Dev hub and interop test

`scripts/gim-hub-dev.sh` builds gim-hub from `GIM_HUB_SRC` at `GIM_HUB_REV`
(see [Integration tests](#integration-tests)) and runs it on the `traefik`
network:

```sh
url=$(scripts/gim-hub-dev.sh start)       # prints http://<ip>:8080
scripts/gim-hub-dev.sh commit             # the hub commit of the image
scripts/gim-hub-dev.sh stop
```

`testing/gim-cli-interop.sh` starts a hub, runs
`apps/gim-cli/tests/interop.rs` (alice, bob and carol through the real binary:
DM, reply, reaction, edit, delete, read receipt, image, group of three,
`listen`, exit codes) in a `rust:1` container and stops the hub. Without
`GIM_HUB_URL` the test is skipped, so `cargo test` stays offline.

```sh
testing/gim-cli-interop.sh
```

### Two-client walkthrough

Two terminals, alice and bob. For an Android interop run, replace either side
with the app (registered against the same hub) and use its handle.

```sh
scripts/gim-hub-dev.sh start              # once; prints the URL
url=$(scripts/gim-hub-dev.sh url)         # in each terminal

# terminal 1: alice
export GIM_PROFILE=/tmp/alice
gim --server "$url" register alice "Alice"
# terminal 2: bob
export GIM_PROFILE=/tmp/bob
gim --server "$url" register bob "Bob"

# alice
gim dm bob                                # prints the chat id
gim send @bob "Leaving Split at 06:00"
# bob
gim wait-for @alice --text "Leaving Split"   # prints the message and its id
gim send @alice "See you in Hvar" --reply <msg>
gim react <msg> 👍
gim read @alice
gim listen --chat @alice                  # live events until Ctrl-C
# alice (bob sees the edit, the image and later messages live)
gim history @bob
gim edit <msg> "Leaving Split at 07:00"
gim send-image @bob bay.png --caption "Stiniva bay"
# bob: Ctrl-C the listen first (one process per profile), then
gim wait-for @alice --text "Stiniva bay"
gim download <msg> /tmp/bay.png
```

## Bindings

```sh
cargo build -p gim-ffi
cargo run -p uniffi-bindgen -- generate --library target/debug/libgim_core.so --language kotlin --out-dir out/kotlin
cargo run -p uniffi-bindgen -- generate --library target/debug/libgim_core.so --language swift --out-dir out/swift
```

Kotlin package `im.g.gimcore`; Swift module `GIMCore` (FFI module `GIMCoreFFI`).

The envelope and media formats are specified in gim-core.

## iOS packaging

```sh
bash scripts/generate-swift.sh      # Linux or macOS: out/swift (Swift sources, header, module.modulemap)
bash scripts/build-xcframework.sh   # macOS + Xcode: out/ios/GIMCore.xcframework{,.zip,.zip.sha256}
```

The xcframework holds arm64 device and arm64 simulator slices only (no Intel-Mac
simulator), built with the size-tuned `release-apple` profile, with embedded
bitcode removed from the prebuilt `compiler_builtins` members, so the zip stays
below 50 MB. See `bindings/apple/README.md` for the output layout, size,
bitcode and link-test notes. `out/` is never committed.

## CI

`.github/workflows/ci.yml`:

- `rust` (every push, pull request, manual run): `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `cargo test`, a `scripts/generate-swift.sh` smoke
  run, and a check that `scripts/generate-bindings.sh` leaves the committed
  bindings unchanged.
- `xcframework` (pushes to `main` and manual runs, macOS): runs
  `scripts/build-xcframework.sh` and uploads `gim-core-ios-<sha>` with the zip,
  its checksum, the Swift sources and `GIM_CORE_VERSION`.
- `integration` (every push, pull request, manual run): checks out gim-hub at
  the pinned commit (`scripts/gim-hub-dev.sh rev`, the `GIM_HUB_REV` default,
  so CI and local runs test the same hub), builds the hub image from that
  checkout (`GIM_HUB_CHECKOUT`, no `git archive`) and runs the single-hub and
  federation suites of `scripts/integration-test.sh` and both modes of
  `testing/gim-cli-interop.sh` on a `gim-ci` network. Hub logs are printed
  on failure.

To build the hub from a directory that already holds `GIM_HUB_REV` locally:

```sh
GIM_HUB_CHECKOUT=/path/to/gim-hub scripts/integration-test.sh
```

A git checkout must have `GIM_HUB_REV` checked out; any other directory (an
archive extraction) is built as is and labelled `GIM_HUB_REV`.

## Android packaging

```sh
scripts/build-android.sh
```

Builds `libgim_core.so` for `arm64-v8a` and `x86_64` plus the Kotlin bindings
into `out/android/` inside a Docker container; see
[bindings/android/README.md](bindings/android/README.md).
