<p align="center">
  <img src="https://raw.githubusercontent.com/dreamzone-cc/ZGALAXY/main/ZGalaxy.svg" alt="ZGALAXY Logo" width="160" height="160" />
</p>

<h1 align="center">ZGALAXY-RS</h1>

<p align="center">
  <b>Sovereign, Memory-Safe, Ultra-High-Performance ZeroTier-Compatible Client & Embedded Controller in 100% Pure Rust</b>
</p>

<p align="center">
  <a href="https://github.com/dreamzone-cc/zgalaxy-rs"><img src="https://img.shields.io/badge/Language-Rust%202021-DEA584?style=for-the-badge&logo=rust&logoColor=white" alt="Rust 2021" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-AGPL--3.0-00ffb7?style=for-the-badge" alt="AGPL-3.0 License" /></a>
  <a href="#"><img src="https://img.shields.io/badge/Architecture-Async%20Tokio%20P2P-00b7ff?style=for-the-badge" alt="Async Tokio" /></a>
  <a href="#"><img src="https://img.shields.io/badge/Dynamic%20IP-Zero--Restart%20In--Memory-ffd700?style=for-the-badge" alt="Zero Restart Dynamic IP" /></a>
  <a href="#"><img src="https://img.shields.io/badge/Integration-ZTNET%20Compatible-brightgreen?style=for-the-badge" alt="ZTNET Compatible" /></a>
</p>

---

## 🌟 Overview

**ZGALAXY-RS** is a next-generation, sovereign clean-room implementation of the ZeroTier network client and embedded network controller written entirely in **pure Rust**. Designed specifically for high-throughput edge environments, private data centers, and community self-hosted topologies, it provides complete memory safety, zero-copy packet processing, and eliminates legacy shortcomings such as daemon restart loops on dynamic IP changes.

---

## ⚡ Key Highlights & Core Differentiators

* 📜 **100% Sovereign Open Source (AGPL-3.0):** Clean-room implementation free from commercial BSL source-available licensing restrictions.
* 🛡️ **Guaranteed Memory Safety:** Zero segmentation faults, zero buffer overflows, zero memory leaks—powered by Rust's ownership model and the Tokio asynchronous runtime.
* ⚡ **Zero-Restart In-Memory Dynamic IP Relinking:** Background async DNS monitoring detects IP drift on domain roots (`dz.dreamzone.cc`) and updates socket routes in memory with **zero packet drops and zero service restarts**.
* 🎛️ **Embedded ZeroTier Controller:** Complete native replacement for `nonfree/controller`, featuring full `FileDB` persistence (`controller.d/`), automatic 6-digit network generation, and IP assignment pools.
* 🔗 **100% External Contract Compatibility:** Seamless drop-in compatibility with **ZTNET**, `zerotier-cli`, and orchestrators on the standard port `9993` with `X-ZT1-Auth` authentication.
* 🚀 **P2P NAT Traversal & Hole Punching:** High-speed STUN/Rendezvous state machine with stateful keep-alive heartbeats for seamless traversal of symmetric and cone NATs.
* 💻 **Cross-Platform:** Native support for Linux TUN, Windows Wintun, macOS UTUN, and ARM64 embedded architectures.

---

## 🏗️ Architecture Overview

```
src/
├── lib.rs                  # Module exports and protocol constants
├── main.rs                 # Daemon entry point, state lifecycle, and CLI runner
├── controller.rs           # Embedded ZeroTier Network Controller (FileDB persistence)
├── controller_api.rs       # Local REST API on Port 9993 (ZTNET & CLI compatible)
├── transport.rs            # Asynchronous UDP wire protocol transport and router
├── nat.rs                  # STUN / Rendezvous NAT traversal and P2P hole-punching
├── route_manager.rs        # Host OS route and IP provisioning (Linux, Windows, macOS)
├── config.rs               # Local node configuration (local.conf and networks.d/)
├── identity.rs             # 40-bit Address derivation, Ed25519 key generation & signing
├── crypto.rs               # Curve25519 (X25519), ChaCha20-Poly1305, Salsa20 AEAD suite
├── packet.rs               # Canonical 28-byte wire protocol packet encoder/decoder
├── world.rs                # Binary Planet/Moon loader and generator
├── resolver.rs             # Native in-memory Async DNS & Dynamic IP watcher
├── tun.rs                  # Virtual TUN/TAP network interface manager
├── peer.rs                 # Peer connection state machine, latency & path discovery
├── network.rs              # Network join/leave and IP assignment manager
└── cli.rs                  # Multi-subcommand CLI and idtool key generator
```

---

## 🚀 Quick Start & Installation

### One-Line Automated Installation

#### Linux (Ubuntu / Debian / RHEL / Arch / Alpine):
```bash
curl -sSL https://raw.githubusercontent.com/dreamzone-cc/zgalaxy-rs/main/install.sh | sudo bash
```

#### Windows (PowerShell as Administrator):
```powershell
irm https://raw.githubusercontent.com/dreamzone-cc/zgalaxy-rs/main/install.ps1 | iex
```

---

### Build from Source

#### Prerequisites:
* Rust 1.75+ (Cargo & Rust toolchain)
* Build tools (`gcc`, `make`, or MSVC on Windows)

```bash
# Clone the repository
git clone https://github.com/dreamzone-cc/zgalaxy-rs.git
cd zgalaxy-rs

# Build high-performance release binary
cargo build --release

# Run daemon
sudo ./target/release/zgalaxy-rs
```

---

## 💻 CLI Usage & Commands

ZGALAXY-RS includes built-in CLI commands compatible with standard ZeroTier workflows:

```bash
# Check node status and address
zgalaxy-cli status
# Output: 200 info 069ae38092 1.3.0 ONLINE

# Join a private network
zgalaxy-cli join 069ae38092000001
# Output: 200 join OK

# List joined networks
zgalaxy-cli listnetworks

# List active connected peers
zgalaxy-cli listpeers

# Leave a network
zgalaxy-cli leave 069ae38092000001
```

### Cryptographic Identity & Moon Utilities (`idtool`):
```bash
# Generate fresh cryptographic identity keypair
zgalaxy-cli idtool generate identity.secret identity.public

# Initialize Moon JSON configuration template
zgalaxy-cli idtool initmoon identity.public > moon.json

# Compile signed binary Moon file
zgalaxy-cli idtool genmoon moon.json
```

---

## 📡 REST API Compatibility (Port 9993)

All requests require authentication using the `X-ZT1-Auth` header populated with the token from `authtoken.secret`.

| Endpoint | Method | Description |
| :--- | :---: | :--- |
| `/status` | `GET` | Retrieve local node status, address, clock, and version |
| `/controller` | `GET` | Retrieve embedded controller status and instance ID |
| `/controller/network` | `GET` / `POST` | List hosted networks or create a new network |
| `/controller/network/:nwid` | `GET` / `POST` / `DELETE` | Read, update, or delete network configuration |
| `/controller/network/:nwid/member` | `GET` | List network member map `{ "<nodeId>": revision }` |
| `/controller/network/:nwid/member/:id` | `GET` / `POST` / `DELETE` | Authorize, inspect, or remove a network member |
| `/network` | `GET` | List locally joined networks |
| `/network/:nwid` | `POST` / `DELETE` | Join or leave a network locally |
| `/peer` | `GET` | List active connected peers, paths, and latency |
| `/metrics` | `GET` | Prometheus formatted telemetry and health metrics |

---

## ⚙️ Configuration

### `local.conf`
Place in `/var/lib/zerotier-one/local.conf`:
```json
{
  "port": 9993,
  "allow_management_from": ["127.0.0.1", "::1"],
  "auto_join_networks": ["069ae38092000001"]
}
```

### Decoupled Dynamic Domain Sources
Configure dynamic root endpoints in `/var/lib/zerotier-one/domains.json`:
```json
[
  {
    "domain": "dz.dreamzone.cc",
    "port": 9993,
    "enabled": true,
    "description": "Primary DreamZone Planet"
  }
]
```

---

## 🤝 Contributing

We welcome community contributions! Please read [`CONTRIBUTING.md`](CONTRIBUTING.md) for details on our code of conduct and development workflows.

---

## 🛡️ Security

For vulnerability disclosures, please review [`SECURITY.md`](SECURITY.md) or email `security@dreamzone.cc`.

---

## 📜 License

This project is licensed under the **GNU Affero General Public License v3.0 (AGPL-3.0)**. See [`LICENSE`](LICENSE) for details.
