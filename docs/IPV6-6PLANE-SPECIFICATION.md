# IPv6 Architecture & 6PLANE Specification

This document provides the technical specification and mathematical derivation for all IPv6 addressing modes implemented in **ZGALAXY-RS**, including **6PLANE**, **RFC 4193 Unique Local IPv6**, and **RFC 7217 Semantically Opaque Interface Identifiers**.

---

## 1. 6PLANE Mesh Routing (`v6AssignMode.6plane`)

6PLANE is an IPv6 overlay routing protocol designed for zero-configuration, route-free peer-to-peer communication.

### A. Subnet Allocation
- **Network Subnet**: `/40` Unique Local Address (ULA) under prefix `fc00::/7`.
- **Member Subnet**: `/80` assigned per node.
- **Host Address**: Host `::1` within the `/80` allocation.

### B. Mathematical Derivation
Given:
- $\text{NWID}$ = 64-bit Network ID
- $\text{NodeID}$ = 40-bit ZeroTier/ZGALAXY Node Address

1. Fold the 64-bit Network ID into 32 bits:
   $$\text{folded} = \text{NWID} \oplus (\text{NWID} \gg 32)$$
2. Construct the 128-bit IPv6 address:
   - Byte 0: `0xfc`
   - Bytes 1..4 (32 bits): $\text{folded}$ (big-endian)
   - Bytes 5..9 (40 bits): $\text{NodeID}$ (big-endian)
   - Bytes 10..14 (40 bits): `0x00, 0x00, 0x00, 0x00, 0x00`
   - Byte 15 (8 bits): `0x01`

### C. ZeroTier C++ Bit-for-Bit Equivalence
`zgalaxy-rs` produces output 100% equivalent to `InetAddress::makeIpv66plane`:
- Example: Network `ef313fb5c9f6817a`, Member `5b07896437`
- Resulting 6PLANE IP: `fc26:c7be:cf5b:789:6437::1`

---

## 2. RFC 4193 Unique Local IPv6 (`v6AssignMode.rfc4193`)

RFC 4193 defines standard Unique Local IPv6 unicast addresses (`fd00::/8`).

### A. Mathematical Derivation
1. Construct 128-bit IPv6 address:
   - Byte 0: `0xfd`
   - Bytes 1..8 (64 bits): $\text{NWID}$ (big-endian)
   - Bytes 9..10 (16 bits): `0x9993` (ZeroTier canonical magic constant)
   - Bytes 11..15 (40 bits): $\text{NodeID}$ (big-endian)

- Example: Network `ef313fb5c9f6817a`, Member `5b07896437`
- Resulting RFC4193 IP: `fdef:313f:b5c9:f681:7a99:935b:789:6437`

---

## 3. RFC 7217 Semantically Opaque Interface Identifiers (`v6AssignMode.zt`)

For custom IPv6 assignment pools (Global Unicast `2001:...` or private ranges), `zgalaxy-rs` implements **RFC 7217**:

$$\text{RID} = \text{HMAC-SHA256}_{K_{\text{controller}}}(\text{Prefix}_{\text{start}}, \text{Prefix}_{\text{end}}, \text{NWID}, \text{MemberID}, \text{DAD}_{\text{counter}})$$

### Privacy & Anti-Tracking Guarantees:
1. **No Hardware / Node Leaks**: Does not embed MAC address or unmasked Node ID.
2. **Cross-Network Unlinkability**: A node connected to multiple networks receives completely uncorrelated addresses.
3. **Cryptographic Stability**: A member receives the identical address upon re-connection.
4. **Collision Resolution**: Duplicate Address Detection (DAD) loop increments `dad_counter` to generate a fresh pseudorandom candidate.
