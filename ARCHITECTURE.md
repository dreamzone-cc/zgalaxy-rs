# ZGALAXY-RS Architecture & Technical Specification

## 1. System Architecture Overview

**ZGALAXY-RS** is an asynchronous, event-driven network daemon designed to provide ZeroTier protocol compatibility with high throughput, memory safety, and decoupled dynamic configuration for community self-hosting.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          ZGALAXY-RS Core Engine                             │
│                                                                             │
│  ┌─────────────────────────┐           ┌─────────────────────────────────┐  │
│  │   Control Plane         │           │   Data Plane (Wire Engine)      │  │
│  │  - REST API (Port 9993) │           │  - Async Tokio UDP Transport    │  │
│  │  - Embedded Controller  │           │  - 28-Byte Canonical Header     │  │
│  │  - FileDB Storage       │           │  - ChaCha20-Poly1305 / Salsa20  │  │
│  │  - CLI Handler          │           │  - TUN / Wintun TAP Router      │  │
│  └───────────┬─────────────┘           └────────────────┬────────────────┘  │
│              │                                          │                   │
│              ▼                                          ▼                   │
│  ┌─────────────────────────┐           ┌─────────────────────────────────┐  │
│  │ Dynamic Resolution      │           │ P2P & NAT Traversal             │  │
│  │  - Multi-Source Config  │           │  - STUN & Rendezvous (0x05)     │  │
│  │  - In-Memory Relinking  │           │  - Path Probing (0x10)          │  │
│  │  - Zero-Restart Drift   │           │  - 25s Stateful Keepalives      │  │
│  └─────────────────────────┘           └─────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Cryptographic Suite & Identity Model

### 40-Bit Node Address Derivation
ZeroTier node addresses are exactly 40 bits (5 bytes / 10 hexadecimal characters). In ZGALAXY-RS, node addresses are derived using the canonical algorithm:

$$\text{Address} = \text{SHA-512}(\text{Ed25519\_PublicKey})[59..64]$$

### Hashcash Proof-of-Work (PoW)
To prevent address spoofing and Sybil attacks on the decentralized mesh, keypair generation requires satisfying the Hashcash condition:
$$\text{Digest}[0] < 17 \quad \text{and} \quad \text{Address}[0] \neq 0\text{xff}$$

### Symmetric Encryption & Key Agreement
* **Ephemeral Key Exchange:** Curve25519 (X25519) Diffie-Hellman.
* **Payload Encryption:** ChaCha20-Poly1305 AEAD / Salsa20-12 stream cipher with 64-bit message authentication codes (MAC).

---

## 3. Wire Protocol & Packet Framing

Packets transmitted over UDP adhere strictly to the canonical 28-byte wire header layout:

| Byte Offset | Field Length | Field Name | Description |
| :---: | :---: | :--- | :--- |
| `0..8` | 8 Bytes | `packet_id` | 64-bit cryptographic initialization vector (IV) / sequence counter |
| `8..13` | 5 Bytes | `dest` | 40-bit Destination ZeroTier Node Address |
| `13..18` | 5 Bytes | `source` | 40-bit Source ZeroTier Node Address |
| `18` | 1 Byte | `flags` | Cipher suite flags and hop counter (0..7) |
| `19..27` | 8 Bytes | `mac` | 64-bit Message Authentication Code (or trusted path ID) |
| `27` | 1 Byte | `verb` | Protocol verb identifier |
| `28..` | Dynamic | `payload` | Verb-specific data payload |

### Canonical Protocol Verbs
* `0x01 HELLO`: Public key announcement and peer discovery.
* `0x02 ERROR`: Protocol error notifications.
* `0x03 OK`: Generic acknowledgment and payload responses.
* `0x04 WHOIS`: Node identity lookup.
* `0x05 RENDEZVOUS`: Upstream NAT traversal and hole-punching mediation.
* `0x06 FRAME`: Layer-2 Ethernet unicast frame.
* `0x07 EXT_FRAME`: Extended Ethernet frame with full MAC and redirection flags.
* `0x08 ECHO`: Latency measurement and path heartbeat.
* `0x0b NETWORK_CONFIG_REQUEST`: Client network configuration request.
* `0x0c NETWORK_CONFIG`: Controller signed network configuration push.
* `0x0d MULTICAST_GATHER`: Multicast group subscriber discovery.
* `0x0e MULTICAST_FRAME`: Multicast Ethernet frame distribution.
* `0x10 PUSH_DIRECT_PATHS`: Dynamic path advertisement and direct route discovery.

---

## 4. Decoupled Dynamic IP & DNS Resolution Subsystem

To support self-hosted root and moon topologies where dynamic WAN IPs frequently change, ZGALAXY-RS eliminates static compile-time IP bindings:

1. **Runtime Decoupling:** Dynamic domains are loaded from `domains.json`, `domain`, environment variables, or API calls.
2. **In-Memory Drift Detection:** The background async resolver checks domain endpoints periodically. When an IP change is detected, it atomically swaps the socket address within `Arc<RwLock<HashMap>>`.
3. **Zero Packet Drops:** Existing UDP session states are maintained without daemon restarts or network disruptions.
4. **Resilient Glitch Handling:** In the event of temporary upstream DNS timeouts, the engine retains the last-known verified good IP address.

---

## 5. Embedded Network Controller Subsystem

ZGALAXY-RS implements a fully native Embedded ZeroTier Controller in pure Rust:

* **Storage Engine (`FileDB`):** Compatible with standard `/var/lib/zerotier-one/controller.d/` directory hierarchies:
  * `/var/lib/zerotier-one/controller.d/network/<nwid>.json`
  * `/var/lib/zerotier-one/controller.d/network/<nwid>/member/<memberId>.json`
* **Auto-Generated Network IDs:** Generates standard 16-hex Network IDs (`<10-hex Controller Node Address> + <6-hex Network Index>`).
* **IP Allocation Pools:** Automatically distributes IPv4 addresses from configured ranges (`ipAssignmentPools`) upon member authorization.
* **ZTNET Integration:** Complete compatibility with the **ZTNET** web management platform via port `9993` REST endpoints.

---

## 6. Operating Modes & Role-Based Authority

To maintain strict security and zero controller footprint on desktop clients:
* **Client Mode (Standard Default):**
  * Node runs strictly as a network client.
  * Embedded controller routes are disabled (`403 Forbidden`).
  * Inbound peer configuration requests over QUIC are rejected.
  * The desktop GUI (`zgalaxy-ui`) is 100% focused on client workflows with no controller controls.
* **Controller Mode (Explicit Activation):**
  * Enabled exclusively for server and ZTNet controller deployments via `--controller` flag, `local.conf`, or `install.sh --controller`.
  * Activates `controller.d/` storage, pool management, member authorization, and QUIC configuration serving.
* **Role Invariants:**
  * If `network_id[..10] == node_id`: Node acts as **Network Owner (Controller)**.
  * If `network_id[..10] != node_id`: Node acts as **Joined Member (Client Mode)**.
  See detailed documentation: [docs/OPERATING-MODES-AND-ROLES.md](docs/OPERATING-MODES-AND-ROLES.md).

---

## 7. IPv6 Addressing & Privacy Architecture

* **6PLANE Mesh Routing (`fc00::/7`):**
  * Subnet: `/40` network with `/80` assigned per node and host `::1`.
  * Fully canonical ZeroTier RFC bit-for-bit derivation: $\text{folded} = \text{nwid} \oplus (\text{nwid} \gg 32)$.
* **RFC 4193 Unique Local IPv6 (`fd00::/8`):**
  * Subnet: `/88` per-network allocation using `fd<nwid>:9993:<node_id>`.
* **RFC 7217 Semantically Opaque Identifiers:**
  * For custom IPv6 pools, interface identifiers are derived using HMAC-SHA256:
    $$\text{IID} = \text{HMAC-SHA256}_{K_{\text{controller}}}(\text{Prefix}, \text{Network ID}, \text{Member ID}, \text{DAD Counter})$$
  * Guarantees anti-tracking privacy across multiple networks, cryptographic stability, and zero MAC/hardware exposure.
  See detailed specification: [docs/IPV6-6PLANE-SPECIFICATION.md](docs/IPV6-6PLANE-SPECIFICATION.md).

