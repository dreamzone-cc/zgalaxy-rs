# ZGALAXY-RS Deep Inspection Report

**Date:** 2026-08-14
**Scope:** `/home/ggonlinux/zt/zgalaxy-rs` (branch `main`, commit `3063c59`)
**Toolchain:** Rust 1.97.1, Cargo 1.97.1
**Result:** All fixes applied and verified. Project builds cleanly, passes all tests, and runs correctly.

---

## 1. Executive Summary

A deep inspection, build, and runtime verification of the ZGALAXY-RS project was performed.
The project is a sovereign ZeroTier-compatible client & embedded controller written in Rust.

The initial source tree **failed to compile** (98 errors). After fixing the issues below, the
project now:

- Compiles cleanly with zero warnings (`cargo build`)
- Passes `cargo clippy` with zero warnings
- Passes all 8 unit tests (`cargo test`)
- Builds a release binary (`cargo build --release`)
- Runs correctly as a daemon (verified in a clean container)
- Serves the REST API on port 9993 with correct authentication
- Persists controller state to disk (FileDB format) and reloads it on restart
- CLI is fully compatible with `zerotier-cli` subcommand names

---

## 2. Critical Issues Found & Fixed

### 2.1 CRITICAL — `Cargo.toml` malformed dependency sections (project would not compile)

**File:** `Cargo.toml`
**Severity:** Critical (build-blocking)

**Problem:**
The `[target.'cfg(windows)'.dependencies]` section was never closed. In TOML, a section
extends until the next `[section]` header. Because the `# Local REST/IPC Server & Web Engine`
comment and the following general dependencies (`axum`, `tower`, `tower-http`, `http`,
`serde`, `serde_json`, `hex`, `byteorder`, `clap`, `url`, `tracing`, `tracing-subscriber`,
`anyhow`, `thiserror`, `ctrlc`) were written *after* the Windows section header without
re-opening `[dependencies]`, **all of these general-purpose crates became Windows-only
dependencies**.

On Linux this produced 98 compile errors of the form:
```
error[E0432]: unresolved import `anyhow`
error[E0433]: cannot find module or crate `serde_json`
error: cannot find attribute `serde`
```

**Fix:**
Reorganized `Cargo.toml` so the general `[dependencies]` section contains all common
crates, and only `tun`/`nix` (Unix) and `wintun`/`windows-sys` (Windows) remain under their
target-specific sections.

### 2.2 HIGH — `PacketType::Pong` did not exist

**File:** `src/packet.rs`, `src/transport.rs`
**Severity:** High (build-blocking)

**Problem:**
`UdpTransport::start_rx_loop` responds to `PacketType::Echo` with a `PacketType::Pong`
packet, but the `Pong` variant was never defined in the `PacketType` enum (nor in the
`From<u8>` / `From<PacketType>` conversions). This caused compile error `E0599`.

**Fix:**
- Added `Pong = 0x0f` to the `PacketType` enum.
- Added `0x0f => PacketType::Pong` to `From<u8>`.
- Added `PacketType::Pong => 0x0f` to `From<PacketType> for u8`.
- Added a comprehensive round-trip test covering **all** 19 packet types.

### 2.3 HIGH — Controller API rejected valid network creation (missing serde defaults)

**File:** `src/controller.rs`
**Severity:** High (functional bug)

**Problem:**
`NetworkConfig` and `MemberRecord` structs had no `#[serde(default)]` on their fields.
Creating a network with a partial JSON payload (exactly how ZTNET and `zerotier-cli`
interact with the controller) failed with HTTP 400:

```
Failed to create network: Failed to deserialize NetworkConfig JSON
Caused by: missing field `mtu`
```

**Fix:**
Added `#[serde(default)]` to all optional fields of `NetworkConfig` and `MemberRecord`,
with sensible defaults for `mtu` (2800) and `multicastLimit` (32).

### 2.4 HIGH — CLI subcommand names broke `zerotier-cli` compatibility

**File:** `src/cli.rs`
**Severity:** High (functional/compat bug)

**Problem:**
clap auto-converts variant names to kebab-case. `ListNetworks` became `list-networks`,
`IdTool` became `id-tool`, etc. This contradicted the README and `zerotier-cli`
compatibility claims:

```
$ zgalaxy-rs listnetworks
error: unrecognized subcommand 'listnetworks'
  tip: some similar subcommands exist: 'list-peers', 'list-networks'
```

**Fix:**
Added explicit `#[command(name = "...")]` attributes (with kebab-case aliases retained for
convenience):
- `listnetworks` (alias `list-networks`)
- `listpeers` (alias `list-peers`)
- `idtool` (alias `id-tool`)
- `initmoon` (alias `init-moon`)
- `genmoon` (alias `gen-moon`)

---

## 3. Functional Bugs Fixed

### 3.1 Network ID generation could produce duplicate IDs

**File:** `src/controller.rs`
**Severity:** Medium

**Problem:**
`save_network` generated the network ID using `len() + 1`. If a network was deleted, the
counter could collide and overwrite an existing network.

**Fix:**
Added `next_network_id()` which scans for the first free counter value, guaranteeing
uniqueness among existing networks.

### 3.2 All authorized members received the same IP address

**File:** `src/controller.rs`
**Severity:** Medium

**Problem:**
When authorizing a member, the code always assigned `pool.ip_range_start` to every member:
```rust
record.ip_assignments.push(pool.ip_range_start.clone());
```
Every member got the same IP → IP conflict in the network.

**Fix:**
Added `next_free_ip()` which walks the pool from start to end and returns the first
address not already assigned to an authorized member. Verified: 3 members in the same
pool received `10.9.0.10`, `10.9.0.11`, `10.9.0.12`.

### 3.3 All joined networks shared the same MAC address and IP

**File:** `src/network.rs`
**Severity:** Medium

**Problem:**
Every joined network hardcoded the same MAC `fe:12:34:56:78:9a` and the same IP
`10.147.17.100/24`, causing conflicts when joining more than one network.

**Fix:**
Added `derive_mac()` and `derive_ipv4()` which deterministically derive a unique
locally-administered MAC and a unique `10.x.x.x` IP from the network ID's tail bytes,
plus `derive_route_network()` for the correct route target.

### 3.4 Daemon crashed with "Permission denied" when system data dir was not writable

**File:** `src/main.rs`
**Severity:** Medium

**Problem:**
`/var/lib/zerotier-one` is chosen as the working directory whenever it exists, regardless
of whether the running user can write to it. Running the daemon as a non-root user with a
root-owned `/var/lib/zerotier-one` caused an immediate crash:
```
Error: Permission denied (os error 13)
```

**Fix:**
The daemon now performs a real write-probe on the system data dir. If the dir exists but
is not writable, it logs a warning and falls back to `./zerotier-var`.

### 3.5 `idtool genmoon` was a no-op stub

**File:** `src/cli.rs`, `src/world.rs`
**Severity:** Medium

**Problem:**
`genmoon` only printed a message and produced no `.moon` file:
```rust
IdToolCommands::GenMoon { moon_json } => {
    println!("Signed moon generated from {}", moon_json);
}
```

**Fix:**
- `genmoon` now reads the moon JSON, builds a `World`, signs it with the secret identity
  (when `signingKey_secret` is provided), and writes a real binary `.moon` file.
- `World::encode()` now emits a self-describing binary format: world type, id, timestamp,
  root count, root identities, and an optional signature.
- `World::parse_binary()` was rewritten to parse the self-describing format.
- Added `test_world_round_trip` and `test_world_parse_truncated` unit tests.

---

## 4. Code Hygiene / Warnings Cleaned

Removed all unused imports and unused variables across the codebase (28 build warnings
and 30+ clippy warnings reduced to zero):

- `src/cli.rs`: removed the dead `reqwest_or_hyper_get()` stub and the unused `client`
  binding; removed `std::io::{Read, Write}` imports from `fetch_json`.
- `src/config.rs`, `src/controller.rs`, `src/crypto.rs`, `src/nat.rs`, `src/network.rs`,
  `src/peer.rs`, `src/resolver.rs`, `src/route_manager.rs`, `src/transport.rs`,
  `src/tun.rs`, `src/world.rs`, `src/main.rs`: removed unused imports and variables.
- `src/controller_api.rs`: removed unused `HashMap` import and unused `headers` parameter
  in `get_metrics`; replaced the useless `format!` with a `const`.
- `src/transport.rs` / `src/controller.rs`: annotated reserved-but-unused struct fields
  with `#[allow(dead_code)]`.
- `src/identity.rs`: removed a needless borrow flagged by clippy.

---

## 5. Runtime Verification (Container Test)

The daemon was built and run inside a clean `rust:1.97-slim` container.

### 5.1 Startup

```
ZGALAXY One — Sovereign Rust Client Daemon v1.3.0
Node Address: f68ee46a39
[ZGALAXY DYNAMIC DNS] Resolved 'dz.dreamzone.cc:9993' -> 154.250.69.161:9993
[ZGALAXY CONTROLLER READY] Loaded 0 networks and 0 member records from disk.
[ZGALAXY UDP TRANSPORT] Bound high-performance UDP router on 0.0.0.0:9993
[ZGALAXY LOCAL REST API] Listening on http://127.0.0.1:9993
```

### 5.2 REST API

| Test | Result |
| :--- | :--- |
| `GET /status` with token | 200 OK, correct JSON |
| `GET /status` without token | 401 Unauthorized |
| `GET /status` wrong token | 401 Unauthorized |
| `GET /controller` | 200 OK |
| `POST /controller/network` (partial payload) | 200 OK — network created |
| `GET /controller/network` | lists network |
| `POST /controller/network/:nwid/member/:id` | 200 OK — member authorized |
| Member IP allocation (3 members, pool .10–.20) | .10, .11, .12 (unique) |
| `DELETE /controller/network/:nwid/member/:id` | 200 OK |
| Controller restart → reload from disk | 1 network, 2 members restored |

### 5.3 CLI

| Command | Result |
| :--- | :--- |
| `zgalaxy-rs status` | `200 info f5acca64ea 1.3.0 ONLINE` |
| `zgalaxy-rs join <nwid>` | `200 join OK` |
| `zgalaxy-rs listnetworks` | works (no dash) |
| `zgalaxy-rs listpeers` | works |
| `zgalaxy-rs leave <nwid>` | `200 leave OK` |
| `zgalaxy-rs idtool generate` | creates `identity.secret` + `identity.public` |
| `zgalaxy-rs idtool initmoon` | outputs valid moon JSON template |
| `zgalaxy-rs idtool genmoon` | writes real signed `moon.moon` file |

---

## 6. Remaining Issues (Not Fixed — Deferred / By Design)

### 6.1 `/metrics` endpoint requires no authentication
The `/metrics` endpoint (Prometheus) intentionally bypasses `X-ZT1-Auth`. The HTTP server
binds to `127.0.0.1` only, so exposure is limited to loopback. If metrics must be secured,
add a token check — but this is standard Prometheus practice and left as-is.

### 6.2 Wire protocol is a simplified custom format, not bit-compatible with ZeroTier C++
The `packet.rs` 28-byte layout and the world/moon binary format are simplified clean-room
representations. They are internally consistent (verified by round-trip tests) but are
**not** guaranteed byte-compatible with the canonical ZeroTier `Packet.hpp` or C++
`World` serialization. Interoperability with genuine ZeroTier planets/moons was not
verified in this environment and would require protocol-level integration testing.

### 6.3 TUN interface is not wired into the runtime path
`src/tun.rs` defines `TunDevice`, but `main.rs` does not create a TUN interface and the
`tun_rx` receiver is dropped (`let (tun_tx, _tun_rx) = ...`). Inbound `Frame` packets are
forwarded to a channel with no active receiver. This is a scaffolding gap for future TUN
integration, not a crash or regression.

### 6.4 `verify()` in identity.rs is not used by the UDP path
The `CryptoEngine` (X25519 / ChaCha20-Poly1305 / Salsa20) exists with passing unit tests,
but the UDP receive path does not yet decrypt/authenticate packets. Packets are currently
processed in plaintext. This is a security hardening item for the future; the crypto
primitives and tests are in place.

### 6.5 STUN/rendezvous keepalive is log-only
`nat.rs` `send_keepalives()` iterates peers and only emits debug logs — it does not yet
send actual ECHO packets over the transport. The transport `send_packet` method exists and
is available for this.

---

## 7. Files Changed

| File | Change |
| :--- | :--- |
| `Cargo.toml` | Fixed malformed dependency sections (critical) |
| `src/packet.rs` | Added `Pong` type + full round-trip tests |
| `src/controller.rs` | serde defaults, unique network IDs, unique IP allocation, cleanup |
| `src/controller_api.rs` | Cleanup, metrics const |
| `src/cli.rs` | CLI names fixed, `genmoon` implemented, cleanup |
| `src/network.rs` | Unique MAC/IP per network, cleanup |
| `src/main.rs` | Writable data-dir detection with fallback, cleanup |
| `src/world.rs` | Self-describing binary format + signature + tests |
| `src/crypto.rs`, `src/identity.rs`, `src/nat.rs`, `src/peer.rs`, `src/resolver.rs`, `src/route_manager.rs`, `src/transport.rs`, `src/tun.rs`, `src/config.rs` | Import/variable cleanup |

---

## 8. Verification Commands

```bash
cargo build                 # zero warnings
cargo build --release       # optimized release build
cargo clippy                # zero warnings
cargo test                  # 8 tests, all passing
```

---

## 9. Recommendations (Next Steps)

1. **Wire the TUN interface** into the runtime path so `Frame` traffic can actually reach
   the host network stack.
2. **Authenticate inbound UDP packets** using the existing `CryptoEngine` (packet MAC +
   decryption) before dispatching to the protocol handlers.
3. **Implement real keepalive/rendezvous sends** in the NAT engine using
   `UdpTransport::send_packet`.
4. **Verify interop against a genuine ZeroTier planet/moon**, and if required, align the
   wire and world binary formats with the canonical ZeroTier C++ serialization.
5. **Add an end-to-end integration test** that exercises the REST API against a running
   daemon instance.

---

# PART II — System Integration Audit (ZGALAXY + ZTNET)

**Date:** 2026-08-14 (second pass)
**Scope:** zgalaxy-rs drop-in integration with:
- The ZGALAXY Planet/Moon infrastructure engine (Node.js, `/home/ggonlinux/zt/zgalaxy`, `dreamzone-cc/ZGALAXY`)
- ZTNET (ZeroTier Web UI for Private Controllers, `sinamics/ztnet`)

All integration fixes below were implemented, verified with live REST flows against a
running daemon, and covered by new unit tests.

---

## 10. ZTNET Integration — Issues Found & Fixed

ZTNET talks to the local controller on port 9993 (configurable) using the
`X-ZT1-Auth` header with the token from `/var/lib/zerotier-one/authtoken.secret`.

### 10.1 CRITICAL — Partial network updates wiped the whole network config

**Problem:** ZTNET updates a single setting at a time by POSTing partial payloads to
`/controller/network/{nwid}` (e.g. `{"name": "..."}`, `{"v4AssignMode": {...}}`,
`{"mtu": 4000}`, `{"dns": {...}}`, `{"routes": [...]}`). zgalaxy-rs deserialized the
partial payload into a fresh `NetworkConfig`, silently resetting **every other field**
to defaults. Renaming a network through ZTNET would have destroyed its routes, IP
pools, MTU, and privacy settings.

**Fix (`src/controller.rs`):** `save_network` now merges the incoming partial payload
over the existing network configuration (top-level field merge), bumps the `revision`
on every save, and preserves `creationTime`.

**Verified:** rename → mtu/pools/routes/v4AssignMode intact; mtu update → name intact;
dns update → everything else intact (revisions 1→2→3→4).

### 10.2 CRITICAL — Partial member updates de-authorized members

**Problem:** ZTNET renames members with `{"name": "..."}` alone, toggles
`noAutoAssignIps`, or updates `ipAssignments` — always as partial payloads. zgalaxy-rs
built a fresh `MemberRecord` from the partial payload, so `authorized` defaulted to
**false**: renaming a member through ZTNET would have silently kicked it off the
network.

**Fix (`src/controller.rs`):** `save_member` merges the partial payload over the
existing member record before applying authorization/auto-assign logic.

**Verified:** rename-only update kept `authorized: true` and the assigned IP.

### 10.3 Missing controller fields used by ZTNET

**Fix (`src/controller.rs`):**
- `NetworkConfig.dns` (ZTNET DNS mutation posts `{"dns": {"domain", "servers"}}`).
- `MemberRecord.name`, `MemberRecord.noAutoAssignIps`, `MemberRecord.capabilities`,
  `MemberRecord.tags` (ZTNET member update fields).
- Auto-assign now respects `noAutoAssignIps` (ZTNET manual-IP toggle).

### 10.4 Peer JSON shape mismatches (`GET /peer`)

**Problem:** ZTNET reads `role` (uppercase), top-level `latency`,
`paths[].address` as `"ip/port"`, `lastSend`/`lastReceive` (camelCase), and
`physicalAddress`. zgalaxy-rs emitted PascalCase roles, `"ip:port"`, snake_case
timestamps, and `latency_ms`.

**Fix (`src/peer.rs`, `src/nat.rs`, `src/cli.rs`):**
- `PeerRole` serialized uppercase (`LEAF`/`MOON`/`PLANET`).
- Path addresses stored/serialized as canonical `"ip/port"` strings.
- `latency_ms` → JSON `latency`; `lastSend`/`lastReceive` camelCase.
- Added `trustedPathId`, `active`, `expired`, `fixed`, `physicalAddress`.
- NAT keepalive worker parses `"ip/port"` back to `SocketAddr`.
- `Address` now serializes as the 10-char hex string (was a raw byte array).

### 10.5 Verified ZTNET REST flows (live)

| ZTNET call | Result |
| :--- | :--- |
| `GET /controller` (version check) | 200, controller: true |
| `GET /status` | address/version/online |
| `POST /controller/network/{addr}______` (network_create) | created with pools/routes/v4AssignMode |
| `GET /controller/network` | `["212556fe37000001"]` |
| `POST /controller/network/{nwid}` partial updates | merge verified |
| `POST .../member/{id}` authorize | auto IP from pool |
| `POST .../member/{id}` rename only | authorized + IP preserved |
| `noAutoAssignIps` member | no IP assigned |
| `GET .../member` | object map `{memberId: revision}` |
| `GET .../member/{id}` | full record |
| stash update (deauth + clear) | works |
| `DELETE .../member/{id}` | works |
| `DELETE /controller/network/{nwid}` | works, disk cleaned |
| `GET /metrics` | Prometheus text |

---

## 11. ZGALAXY Engine Integration — Issues Found & Fixed

The ZGALAXY engine (Node.js) drives the node through the zerotier-idtool CLI and the
`/var/lib/zerotier-one` data directory. zgalaxy-rs is the drop-in replacement.

### 11.1 CRITICAL — `idtool genmoon` produced the wrong file name

**Problem:** ZGALAXY's `MoonService.createMoon` and `entrypoint.sh` expect genmoon to
emit `<worldId-16-hex>.moon` (e.g. `000000069ae38092.moon`). zgalaxy-rs wrote
`moon.moon`, so ZGALAXY failed with "Failed to locate generated .moon file".

**Fix (`src/cli.rs`):** genmoon now writes `format!("{:016x}.moon", world_id)`.
**Verified:** `000000845eda834b.moon` produced.

### 11.2 CRITICAL — `idtool initmoon` emitted an empty signing secret

**Problem:** ZGALAXY requires `signingKey` plus a non-empty `signingKey_SECRET` /
`signingKey_secret`; an empty secret makes `ensureMoonJsonKeys` fail permanently.

**Fix (`src/cli.rs`):** initmoon reads `identity.secret` next to `identity.public`
(real idtool behavior) and emits the full secret identity string under
`signingKey_SECRET`, plus `worldType: "moon"` and `updatesMustBeSigned`.

### 11.3 `genmoon` ignored root stableEndpoints

**Problem:** ZGALAXY writes endpoints like `"197.202.16.121/9994"` into
`moon.json roots[0].stableEndpoints`; genmoon hardcoded `dz.dreamzone.cc:9993`.

**Fix (`src/cli.rs`, `src/world.rs`):** genmoon reads endpoints from the JSON (both
`roots[].id` and `roots[].identity` accepted). The world binary format now stores the
endpoints per root; `parse_binary` reads them back (round-trip test updated).

### 11.4 Daemon ignored the ZGALAXY port convention

**Problem:** ZGALAXY runs the node with the port from `/app/config/zerotier-one.port`
(`ZT_PORT=9994`). zgalaxy-rs hardcoded 9993 for both UDP and REST.

**Fix (`src/main.rs`):** port resolution = local.conf → `./config/zerotier-one.port` →
`ZT_PORT` env → 9993. The REST control plane now binds the **same** port as the UDP
transport (matching ZeroTier behavior).
**Verified:** daemon bound UDP + REST on 9994 with a `config/zerotier-one.port` file.

### 11.5 Resolver ignored ZGALAXY config files

**Fix (`src/resolver.rs`, `src/main.rs`):** `load_sources` now also reads
`./config/domain` and `./config/domains.json` (ZGALAXY format — no port field, so the
daemon's port is applied as default). `DomainEndpointConfig` gained serde defaults for
partial configs.

### 11.6 ZGALAXY engine: identity validation used SHA-384 (fixed in ZGALAXY repo)

**Problem:** `ZGALAXY/src/services/identityService.ts` verified `identity.public` by
hashing the public key with **SHA-384**, while ZeroTier and zgalaxy-rs derive the
address from **SHA-512** (last 5 bytes). Every zgalaxy-rs / real-ZeroTier identity was
reported as `MISMATCH`.

**Fix (`/home/ggonlinux/zt/zgalaxy/src/services/identityService.ts`):** `sha384` →
`sha512`.
**Verified:** node check — SHA-512 derives `845eda834b` = stored address (match);
old SHA-384 logic would have produced `7acb5755f4` (mismatch). `tsc --noEmit` passes.

### 11.7 Secret file permissions (security hardening)

**Fix (`src/main.rs`, `src/cli.rs`):** `identity.secret` and `authtoken.secret` are now
written with `0600` permissions on Unix (canonical ZeroTier behavior).
**Verified:** `-rw-------` on both files in a fresh container.

### 11.8 Verified ZGALAXY idtool flows (live)

| ZGALAXY call | Result |
| :--- | :--- |
| `idtool generate identity.secret identity.public` | files created, secret 0600 |
| `idtool initmoon identity.public` | `signingKey` + non-empty `signingKey_SECRET` |
| `idtool genmoon moon.json` (slash endpoints) | `000000845eda834b.moon`, signed, endpoints embedded |

---

## 12. Remaining Known Gaps (Not Fixed — Out of Scope)

1. **Wire protocol & world binary are not bit-compatible with ZeroTier C++.** The moon
   file produced by genmoon uses a clean-room binary layout, so it cannot yet be
   consumed by stock ZeroTier clients. Full byte-compatibility requires porting the
   canonical ZeroTier World serialization (protobuf-like encoding + signature over
   fields) — a larger protocol effort.
2. **`mkmoonworld-x86_64` remains required for planets.** The ZGALAXY engine still
   calls the C `mkmoonworld-x86_64` binary for `world.bin` (planet) generation;
   zgalaxy-rs only replaces `zerotier-idtool` flows today.
3. **REST binds to 127.0.0.1** — same as real ZeroTier's default. ZTNET deployments
   using `network_mode: host` work out of the box; non-host deployments need a
   configurable bind address (future `allow_management_from` handling).
4. **UDP packets are not yet encrypted/authenticated** with the CryptoEngine; the
   crypto primitives and tests are in place but the wire path is plaintext.

---

## 13. Updated Verification

```bash
cargo build                 # zero warnings
cargo build --release       # optimized
cargo clippy                # zero warnings
cargo test                  # 14 tests passing (merge, noAutoAssignIps, peer JSON, world round-trip, ...)
tsc --noEmit                # ZGALAXY engine compiles after SHA-512 fix
```
