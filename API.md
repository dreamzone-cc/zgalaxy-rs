# ZGALAXY-RS REST API Reference

The ZGALAXY-RS daemon exposes a high-performance local REST control plane on port `9993` (or configured port) compatible with official ZeroTier clients, orchestration tools, and **ZTNET**.

---

## 🔐 Authentication

All API requests must include the authentication token from `/var/lib/zerotier-one/authtoken.secret`.

### Supported Headers:
```http
X-ZT1-Auth: <secret_token>
```
Or:
```http
Authorization: Bearer <secret_token>
```

---

## 📡 Node Status Endpoints

### 1. Get Node Status
```http
GET /status
```
**Response (200 OK):**
```json
{
  "address": "069ae38092",
  "publicIdentity": "069ae38092:0:abcdef123456...",
  "planetWorldId": 0,
  "planetWorldTimestamp": 1723626000000,
  "version": "1.3.0",
  "versionMajor": 1,
  "versionMinor": 3,
  "versionRev": 0,
  "clock": 1723626000000,
  "online": true,
  "tcpFallbackActive": false
}
```

---

### 2. Get Controller Status
```http
GET /controller
```
**Response (200 OK):**
```json
{
  "controller": true,
  "apiVersion": 1,
  "clock": 1723626000,
  "instanceId": "zgalaxy_rs_069ae38092"
}
```

---

### 3. Get Prometheus Metrics
```http
GET /metrics
```
**Response (200 OK - `text/plain`):**
```text
# HELP zgalaxy_controller_status Status of ZGALAXY controller
# TYPE zgalaxy_controller_status gauge
zgalaxy_controller_status 1
# HELP zgalaxy_version Controller version
# TYPE zgalaxy_version gauge
zgalaxy_version 1
```

---

## 🌐 Network Controller Endpoints (ZTNET Compatible)

### 4. List Hosted Networks
```http
GET /controller/network
```
**Response (200 OK):**
```json
[
  "069ae38092000001",
  "069ae38092000002"
]
```

---

### 5. Create or Update Network
```http
POST /controller/network/:nwid
```
*Note: Wildcard `______` (e.g. `/controller/network/069ae38092______`) is automatically resolved to the next sequential network ID.*

**Request Payload:**
```json
{
  "name": "Production Mesh",
  "private": true,
  "mtu": 2800,
  "routes": [
    { "target": "10.147.17.0/24", "via": null }
  ],
  "ipAssignmentPools": [
    { "ipRangeStart": "10.147.17.1", "ipRangeEnd": "10.147.17.254" }
  ],
  "v4AssignMode": { "zt": true }
}
```

---

### 6. Get Network Details
```http
GET /controller/network/:nwid
```

---

### 7. Delete Network
```http
DELETE /controller/network/:nwid
```

---

### 8. List Network Members
```http
GET /controller/network/:nwid/member
```
**Response (200 OK):**
```json
{
  "1234567890": 1,
  "abcdef9876": 2
}
```

---

### 9. Authorize and Update Member
```http
POST /controller/network/:nwid/member/:memberId
```
**Request Payload:**
```json
{
  "authorized": true,
  "activeBridge": false,
  "ipAssignments": ["10.147.17.10"]
}
```

---

### 10. Delete Member
```http
DELETE /controller/network/:nwid/member/:memberId
```

---

## 👥 Peer & Local Network Endpoints

### 11. List Active Peers
```http
GET /peer
```
**Response (200 OK):**
```json
[
  {
    "address": "1234567890",
    "version": "1.3.0",
    "latency_ms": 14,
    "role": "LEAF",
    "paths": [
      {
        "address": "198.51.100.25:9993",
        "last_send": 1723626000000,
        "last_receive": 1723626000000,
        "is_preferred": true
      }
    ]
  }
]
```

---

### 12. List Locally Joined Networks
```http
GET /network
```

---

### 13. Join Network
```http
POST /network/:nwid
```

---

### 14. Leave Network
```http
DELETE /network/:nwid
```

---

## 🌍 Dynamic Domain Management Endpoints

### 15. List Dynamic Domains
```http
GET /api/v1/domains
```

### 16. Add Dynamic Domain
```http
POST /api/v1/domains
```
**Request Payload:**
```json
{
  "domain": "myplanet.org",
  "port": 9993,
  "description": "Backup Community Root"
}
```

### 17. Remove Dynamic Domain
```http
DELETE /api/v1/domains/:domain
```

### 18. Sync Dynamic Domains Now
```http
POST /api/v1/domains/sync
```
