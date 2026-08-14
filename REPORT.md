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
