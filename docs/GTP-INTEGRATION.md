<!-- اقرأ docs/CONTINUE_HERE.md أولاً — دليل الاستئناف المركزي -->
# تكامل GTP — حالة الجاهزية وخطة الإضافة

> المرجع التصميمي الكامل: `zgalaxy_rs_gtp_direct_relay_technical_spec_v3.md` في
> مستودع [GTP-rs](https://github.com/dreamzone-cc/GTP-rs) (الإصدار 0.4.0 وقت الكتابة).
> هذا الملف يسجّل ما نُفّذ فعلاً في zgalaxy-rs، والعقد الذي سيُكتب عليه
> `GtpTransport`، وما تبقى بالترتيب.

## 1. القرار المعماري باختصار

سؤالان منفصلان:

| السؤال | الجواب | النوع في الكود |
|---|---|---|
| كيف أصل إلى النظير؟ | مباشر أو عبر relay | `mesh::PathMode { Direct, Relay }` |
| أي نقل داخل المسار؟ | QUIC أو GTP | `mesh::TransportKind { Quic, Gtp, Udp }` |

GTP **backend نقل** فقط. لا يملك المتحكم ولا NAT ولا الـrelay، ولا يحل تشفيره
محل هوية ZGALAXY: التحدي/الإثبات ed25519 (أ3) يجري فوق أي نقل كما هو.

## 2. ما نُفّذ (المراحل 1–4 من خطة GTP-rs §55)

```text
QuicTransport ──(QuicMesh: impl MeshTransport)──┐
                                                ├─► TransportEvent ─► ControlEngine
GtpTransport  ──(impl MeshTransport) [قادم]─────┘        │                │
                                                       Frame          Control
                                                         │                │
                                                     L2Switch→TAP   PeerSessions + EmbeddedController
```

| المرحلة | الحالة | أين |
|---|---|---|
| 1 — فصل رسائل التحكم عن QUIC | ✅ | `src/control.rs` (نفس صيغة السلك: طول 4 بايت + JSON) |
| 2 — تجريد النقل | ✅ | `src/mesh/{traits,events,capabilities}.rs` |
| 3 — محوّل QUIC بلا تغيير سلوك على السلك | ✅ | `QuicMesh` في `src/quic.rs` |
| 4 — ControlEngine خارج main.rs | ✅ | `src/control_engine.rs` (+ `src/mesh/session.rs` لربط الهوية) |
| 5 — PathManager (Direct/Relay) | ⏳ | `PathMode` معرَّف فقط |
| 6 — فك NAT عن QUIC | ✅ جزئياً | `nat.rs` يتعامل مع `dyn MeshTransport` |
| 7+ — GTP-rs نفسه | ⏳ | انظر §4 |

`main.rs` لم يعد يحتوي أي منطق تحكم خاص بـQUIC: يربط النقل، ثم يشغّل
`ControlEngine::run` و`forward_outbound` و`rtt_prober` و`config_sync_loop`.

### اختيار النقل
`local.conf` → `settings.transportMode`: ‏`"quic"` (افتراضي) | `"gtp"` | `"udp"`.
اختيار `gtp` اليوم يُسجَّل تحذيراً ويرجع إلى QUIC (`TransportKind::is_available`)
حتى يُربط الـbackend — لا انهيار ولا تشغيل صامت بنقل خاطئ.

### MTU من قدرات النقل لا من ثابت
`TransportCapabilities { max_datagram_payload, max_frame, .. }`، و
`interface_mtu() = min(2800, max_frame - 14)`. ‏QUIC يرسل الإطار الأكبر من حد
الـdatagram على uni-stream مستقل (متوافق مع الأقران القدامى: يتوقف تلقائياً
لكل اتصال لا يقبل streams)، فيحمل 2800 كاملة. ‏GTP سيحتاج الشيء نفسه (§4.3).

## 3. العقد الذي سيُكتب عليه GtpTransport

```rust
#[async_trait]
pub trait MeshTransport: Send + Sync {
    fn kind(&self) -> TransportKind;
    fn capabilities(&self) -> TransportCapabilities;
    fn local_addr(&self) -> Result<SocketAddr>;
    async fn connect(&self, remote: SocketAddr) -> Result<()>;
    async fn send_frame(&self, remote: SocketAddr, frame: Bytes) -> Result<()>;   // لا يطلب اتصالاً جديداً أبداً
    async fn send_control(&self, remote: SocketAddr, m: &ControlMessage) -> Result<()>;
    async fn connected_peers(&self) -> Vec<SocketAddr>;
    async fn active_generation(&self, remote: SocketAddr) -> Option<SessionGeneration>;
    async fn close(&self, remote: SocketAddr);
}
```

التزامات الأحداث (`TransportEvent`) التي يجب أن يحترمها أي backend — كلها مغطاة
باختبارات QUIC ويجب تكرارها لـGTP:

1. `Connected { remote, generation }` يصل **قبل** أي Frame/Control من تلك الجلسة
   (المحرك يصفّر حالة الهوية عنده ثم يرسل `NodeAnnounce`).
2. جلسة جديدة لنفس `remote` تحصل على `generation` جديد وتستبدل القديمة.
3. `Disconnected` يحمل generation الجلسة المنتهية؛ المحرك يتجاهله إن كانت جلسة أحدث نشطة.
4. مفاتيح `remote` بصيغة قانونية (IPv4 لا `::ffff:a.b.c.d`) — راجع `quic::canonical_endpoint`.
5. `send_frame` لا يطلب اتصالاً (data plane لا يتجمد على مهلة handshake).

## 4. خريطة GTP-rs 0.4.0 → MeshTransport (تحقق من الـAPI قبل البناء)

من `crates/gtp` (feature `tokio`) و`crates/gtp-runtime-tokio`:

| حاجة zgalaxy | GTP-rs |
|---|---|
| ربط endpoint | `GtpEndpoint::bind(addr)` / `bind_with_static_identity` |
| قبول جلسة | `GtpEndpoint::accept() -> Option<AsyncGtpConnection>` |
| طلب جلسة | `GtpEndpoint::connect(ConnectionId, peer_addr, secure=true)` |
| إطار L2 (غير موثوق) | `AsyncGtpConnection::send_unreliable(payload, PriorityTier::P2WorldState, None)` |
| رسالة تحكم (موثوق مرتب) | `send_reliable_ordered(.., OrderedGroupId(0), PriorityTier::P0Control)` |
| Ping/Pong | `PriorityTier::P0Control` (أو `send_ping` الأصلي لقياس RTT النقل) |
| الاستقبال | `recv() -> ReceivedMessage { class, payload }` — نفرّق بالـ`class` |
| قياس | `feedback()` / `query_metrics()` → يمكن أن يغذي `/peer` latency |
| ترحيل المسار | `trigger_path_challenge(new_addr, nonce)` → `path_migration: true` |
| إغلاق | `graceful_close(code, reason)` |

### 4.1 التوجيه الداخلي للرسائل
- `ReceivedMessage.class == Unreliable` → `TransportEvent::Frame`.
- `ReliableOrdered` → فك `control::decode` → `TransportEvent::Control`.
- `ConnectionId`: مشتق من (عنوان العقدتين + عداد) أو عشوائي 64-bit؛ الهوية
  الحقيقية تبقى من أ3 لا من CID.

### 4.2 الأمان
- `secure=true` إلزامي (الـplaintext في GTP-rs محصور بـfeature اختبار).
- تشفير GTP يحمي القناة؛ **أ3 يبقى إلزامياً** فوقه (مواصفة GTP-rs §27).
- `bind_with_static_identity` + `set_trusted_server_static` يسمحان لاحقاً بتثبيت
  مفتاح X25519 للمتحكم — خطوة مستقلة بعد العمل الأساسي.

### 4.3 الحجم (نقطة حرجة)
`GtpConnection::max_message_payload()` = ‏`min_mtu − header − AEAD tag − 27`
(≈ 1150 بايت عند min_mtu=1200) و`send_unreliable` يرفض ما فوقه بـ
`PayloadTooLarge`. إطار 2814 بايت (MTU 2800) لا يمر Unreliable. الخيارات:
1. **(مقترح)** نفس نمط QUIC: ما يتجاوز الحد يُرسل `send_reliable_unordered`
   (GTP-rs يجزّئ الرسائل الموثوقة عبر `fragment_budget`) — بلا HOL بين الإطارات.
2. خفض MTU الواجهة عند GTP إلى `max_message_payload − 14` — يكسر IPv6 (<1280).

القيمة الفعلية تُقرأ من الاتصال، لا تُفترض.

### 4.4 الاعتماد
GTP-rs workspace بـ13 crate، ‏`rust-version = 1.85`، ‏tokio 1.40 (متوافق مع 1.38+ هنا).
يُضاف كـfeature اختياري ليبقى البناء الافتراضي بلا شبكة إضافية:

```toml
[features]
gtp = ["dep:gtp"]

[dependencies]
gtp = { git = "https://github.com/dreamzone-cc/GTP-rs", tag = "v0.4.0", features = ["tokio"], optional = true }
```
وعندها `TransportKind::Gtp.is_available()` يصبح `cfg!(feature = "gtp")`.

## 5. الخطوات التالية بالترتيب

1. **المرحلة 7**: إضافة الاعتماد تحت feature + اختبار دخان لـAPI الفعلي
   (bind/connect/accept/unreliable/reliable_ordered) — لا نعتمد على API مفترض.
2. **المرحلة 8**: `src/gtp_transport.rs` ‏(`GtpMesh: impl MeshTransport`) + نسخ
   `tests/quic_transport_test.rs` و`tests/mesh_engine_e2e.rs` لتعمل على GTP
   (نفس الالتزامات §3). الثاني يغطي الدورة كاملة: إثبات → join → تخويل → عنوان → إطارات.
3. **المرحلة 9**: متحكم عبر GTP (يعمل تلقائياً — ControlEngine لا يعرف النقل).
4. **المرحلة 5/10+**: ‏PathManager + relay (مواصفة GTP-rs §10–§19، §44–§49).
5. تشغيل مزدوج QUIC+GTP على نفس العقدة (منفذان) لترحيل تدريجي بلا كسر D1.

## 6. ما لا يتغير
- صيغة رسائل التحكم على السلك، وصيغة datagrams الإطارات، وتسلسل أ3.
- REST/ztnet، ‏controller.d، ‏networks.d.
- `src/transport.rs` (جسر UDP القديم) — لا إعادة تسمية مع عمل GTP (مواصفة §54).
