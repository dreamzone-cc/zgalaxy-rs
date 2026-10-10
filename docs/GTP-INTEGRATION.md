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
GtpTransport  ──(GtpMesh: impl MeshTransport)──┘        │                │
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
| 7 — اعتماد GTP-rs + اختبار دخان للـAPI الفعلي | ✅ | feature `gtp`، `tests/gtp_api_smoke.rs` — انظر §4.5 |
| 8 — `GtpMesh` + نفس اختبارات QUIC + ربط الـdaemon | ✅ | `src/gtp_transport.rs` — انظر §4.6 |

`main.rs` لم يعد يحتوي أي منطق تحكم خاص بـQUIC: يربط النقل، ثم يشغّل
`ControlEngine::run` و`forward_outbound` و`rtt_prober` و`config_sync_loop`.

### اختيار النقل
`local.conf` → `settings.transportMode`: ‏`"quic"` (افتراضي) | `"gtp"` | `"udp"`.
`gtp` يعمل في بناء `cargo build --features gtp`. في بناء بدونه يُسجَّل تحذير ويرجع
إلى QUIC (`TransportKind::is_available`) — لا انهيار ولا تشغيل صامت بنقل خاطئ.
فشل ربط منفذ GTP يرجع إلى QUIC أيضاً.

```json
{ "settings": { "transportMode": "gtp" } }
```
كل العقد في الشبكة يجب أن تستخدم النقل نفسه: QUIC وGTP لا يتفاهمان على السلك (§5 بند 5).

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

## 4. خريطة GTP-rs 0.4.0 → MeshTransport (مُتحقَّق منها في المرحلة 7)

من `crates/gtp` (feature `tokio`) و`crates/gtp-runtime-tokio`:

| حاجة zgalaxy | GTP-rs |
|---|---|
| ربط endpoint | `GtpEndpoint::bind(addr)` / `bind_with_static_identity` |
| قبول جلسة | `GtpEndpoint::accept() -> Option<AsyncGtpConnection>` |
| طلب جلسة | `GtpEndpoint::connect(ConnectionId, peer_addr, secure=true)` |
| إطار L2 (غير موثوق) | `AsyncGtpConnection::send_unreliable(payload, PriorityTier::P2WorldState)` (وسيطان فقط) |
| رسالة تحكم (موثوق مرتب) | `send_reliable_ordered(OrderedGroupId(1), payload, PriorityTier::P0Control)` — **ليس 0**، انظر §4.5 |
| Ping/Pong | `PriorityTier::P0Control` (أو `send_ping` الأصلي لقياس RTT النقل) |
| الاستقبال | `recv(&mut self) -> Option<ReceivedMessage { class, payload }>` — نفرّق بالـ`class`؛ `None` = انتهت الجلسة |
| قياس | `feedback()` / `query_metrics()` → يمكن أن يغذي `/peer` latency |
| ترحيل المسار | `trigger_path_challenge(new_addr, nonce)` → `path_migration: true` |
| إغلاق | `graceful_close(code, reason)` |

### 4.1 التوجيه الداخلي للرسائل
- `ReceivedMessage.class == Unreliable` → `TransportEvent::Frame`.
- `ReliableOrdered { group_id: 1, .. }` → فك `control::decode` → `TransportEvent::Control`.
- `ReliableUnordered` → `TransportEvent::Frame` (الإطارات الكبيرة، §4.3).
- `ConnectionId`: مشتق من (عنوان العقدتين + عداد) أو عشوائي 64-bit؛ الهوية
  الحقيقية تبقى من أ3 لا من CID.

### 4.2 الأمان
- `secure=true` إلزامي (الـplaintext في GTP-rs محصور بـfeature اختبار).
- تشفير GTP يحمي القناة؛ **أ3 يبقى إلزامياً** فوقه (مواصفة GTP-rs §27).
- `bind_with_static_identity` + `set_trusted_server_static` يسمحان لاحقاً بتثبيت
  مفتاح X25519 للمتحكم — خطوة مستقلة بعد العمل الأساسي.

### 4.3 الحجم (نقطة حرجة)
`GtpConnection::max_message_payload()` = ‏`min_mtu − header − AEAD tag − 27`
(**1133 بايت مقيسة** على loopback مع `competitive_fps`) و`send_unreliable` يرفض ما فوقه بـ
`PayloadTooLarge`. إطار 2814 بايت (MTU 2800) لا يمر Unreliable. الخيارات:
1. **(مقترح)** نفس نمط QUIC: ما يتجاوز الحد يُرسل `send_reliable_unordered`
   (GTP-rs يجزّئ الرسائل الموثوقة عبر `fragment_budget`) — بلا HOL بين الإطارات.
2. خفض MTU الواجهة عند GTP إلى `max_message_payload − 14` — يكسر IPv6 (<1280).

القيمة الفعلية تُقرأ من الاتصال، لا تُفترض. الخيار 1 مُتحقَّق منه: إطار 2814 بايت
يصل سليماً عبر `ReliableUnordered` في ~1ms على loopback، ورسالة 64KiB (حد الإطار) تصل كذلك.

### 4.5 ما كشفه اختبار الدخان (المرحلة 7)
`cargo test --features gtp --test gtp_api_smoke -- --nocapture`

| الحقيقة | الأثر على `GtpMesh` |
|---|---|
| `OrderedGroupId(0)` محجوز للتسليم **غير المرتب** (يصل `ReliableUnordered`) | التحكم على المجموعة 1؛ لو استُخدمت 0 لضاع الترتيب بصمت |
| `recv` يحتاج `&mut self` والإرسال `&self`، والحقول الداخلية `pub(crate)` | مهمة واحدة تملك كل اتصال: `select!` بين `recv()` وقناة أوامر الإرسال |
| سقف Unreliable = 1133 بايت | ما فوقه → `ReliableUnordered` (حتى 64KiB) |
| `graceful_close` من طرف → `recv()` يرجع `None` عند الآخر فوراً (<1ms) | `None` → `TransportEvent::Disconnected` |
| **إسقاط** `AsyncGtpConnection` و`GtpEndpoint` لا يغلق الجلسة: مهام GTP-rs تبقى وتجيب الـkeepalive | `close()` يجب أن يستدعي `graceful_close` صراحة، ويُغلق كل الجلسات عند الإيقاف |
| نظير معطّل (بلا CLOSE) يُكتشف بعد 15.0s (`competitive_fps`: idle 15s، keepalive 1s) | مطابق لمهلة QUIC (ب4)، لا تغيير في سلوك الاسترداد |
| `connect` يستخدم `competitive_fps()` ثابتاً — لا يقبل `GtpConfig` | لا يمكن ضبط المهلة/الـMTU من zgalaxy حالياً |
| نفس الـendpoint يقبل ويطلب جلسات معاً، والجلسات مستقلة | منفذ واحد لكل عقدة كما في QUIC |
| الـCID يختاره الطالب ويراه القابل كما هو | CID عشوائي 64-bit لكل dial؛ تصادمه لا يمس الهوية (أ3) |
| `peer_addr()` عند القابل = عنوان socket الطالب | مفتاح `remote` بعد `canonical_endpoint` (الالتزام 4) |

### 4.4 الاعتماد
GTP-rs workspace بـ13 crate، ‏`rust-version = 1.85`، ‏tokio 1.40 (متوافق مع 1.38+ هنا).
يُضاف كـfeature اختياري ليبقى البناء الافتراضي بلا شبكة إضافية:

```toml
[features]
gtp = ["dep:gtp"]

[dependencies]
gtp = { git = "https://github.com/dreamzone-cc/GTP-rs", tag = "v0.4.0", features = ["tokio"], optional = true }
```
المُنفَّذ فعلاً يثبّت `rev = "547c46d…"` (commit الوسم v0.4.0، فالوسم annotated وقد يُنقل)
بدلاً من `tag`. البناء الافتراضي لا يسحب أي crate من GTP.
`TransportKind::Gtp.is_available()` يبقى `false` حتى يوجد `GtpMesh` (المرحلة 8)،
ثم يصبح `cfg!(feature = "gtp")` — تفعيله قبل ذلك يعني اختيار نقل غير موجود.

### 4.6 GtpMesh (المرحلة 8) — كيف بُني وما تحقق
- **مهمة واحدة لكل جلسة** (`drive_session`): `select!` بين `recv()` وقناة أوامر
  (Frame / Control / Close). الإطار `try_send` (طابور ممتلئ = إسقاط كبطاقة شبكة،
  لا انتظار)؛ التحكم ينتظر نتيجة الإرسال عبر oneshot.
- **الإطار**: `send_unreliable`؛ عند `PayloadTooLarge { max }` يُحفظ الحد لتلك الجلسة
  وما فوقه يذهب `send_reliable_unordered`. التحكم: `send_reliable_ordered` على `CONTROL_GROUP = 1`.
- **الأجيال**: عداد ذري خاص بالنقل (GTP لا يملك `stable_id`)؛ جلسة جديدة لنفس
  `remote` تستبدل القديمة وتغلقها بـCLOSE صريح.
- **الإغلاق**: `close()` يرسل CLOSE وينتظر وضعه في الطابور؛ `close_all()` عند
  الإيقاف + مهلة 200ms ليخرج الـCLOSE فعلاً قبل انتهاء العملية. قبل المهلة شوهد حياً
  أن النظير لا يعلم إلا بعد 15s.
- **CID**: عشوائي 64-bit لكل dial.

التحقق:
| | QUIC | GTP |
|---|---|---|
| اختبارات النقل (تحكم مرتب، إطار صغير، 2814B، رفض >64KiB، أجيال، إغلاق، dual-stack) | ✅ | ✅ `gtp_transport_test.rs` |
| الدورة الكاملة (إثبات → join → رفض → تخويل → عنوان → إطارات 2814B) | ✅ | ✅ |
| حالات سلبية مُتحقَّق منها بالـmutation (config من غير المتحكم، إطار من جلسة غير مُثبتة) | ✅ | ✅ |
| حيّ: خدمتان على loopback — join → أول رد | 0.09s | 0.11s |
| حيّ: authorize → OK | 0.43s | 0.45s |
| حيّ: إعادة تشغيل المتحكم (SIGINT) → config جديد عند العميل | ~13.5s | **0.9–1.5s** (4/4) |

الفرق الأخير لصالح GTP لأن `close_all` يرسل CLOSE عند الإيقاف، بينما QUIC يخرج
بلا CLOSE فينتظر العميل مهلة الخمول. نفس المعالجة ممكنة لـQUIC
(`endpoint.close` + `wait_idle`) — مسجّلة في PENDING_WORK.

## 5. الخطوات التالية بالترتيب

1. ~~**المرحلة 7**~~ ✅: الاعتماد تحت feature `gtp` (مثبّت على commit الوسم v0.4.0) + `tests/gtp_api_smoke.rs`.
2. ~~**المرحلة 8**~~ ✅: `GtpMesh` + `tests/gtp_transport_test.rs` + الدورة الكاملة
   (`tests/common/mod.rs` تشغّلها لـQUIC وGTP معاً) + ربط `main.rs`.
3. ~~**المرحلة 9**~~ ✅ (ضمناً): المتحكم يعمل عبر GTP بلا أي تغيير — مُتحقَّق حياً (§4.6).
4. **المرحلة 5/10+**: ‏PathManager + relay (مواصفة GTP-rs §10–§19، §44–§49).
5. تشغيل مزدوج QUIC+GTP على نفس العقدة (منفذان) لترحيل تدريجي بلا كسر D1.

## 6. ما لا يتغير
- صيغة رسائل التحكم على السلك، وصيغة datagrams الإطارات، وتسلسل أ3.
- REST/ztnet، ‏controller.d، ‏networks.d.
- `src/transport.rs` (جسر UDP القديم) — لا إعادة تسمية مع عمل GTP (مواصفة §54).
