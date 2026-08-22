# تحليل مرجعي: ZeroTierOne + zerotier-go → قرارات المنفذ إلى zgalaxy-rs (QUIC)

مصادر: مستنسخان في `~/zgalaxy-work/ZeroTierOne` و`~/zgalaxy-work/zerotier-go`.

## 1. REST API المحلية التي تتوقعها أي واجهة Tray (ZeroTierOne service/OneService.cpp:2013-2990)

- مصادقة: loopback فقط افتراضياً (+ `allowManagementFrom`)، توكن من `authtoken.secret` عبر `X-ZT1-Auth`/`?auth=`/`Bearer`. نقاط عامة فقط: `/sso`, `/health`.
- `GET /status`: address, publicIdentity, online, version*, clock, config.settings.{primaryPort,listeningOn[],...}, planetWorldId/Timestamp.
- `GET /network` → مصفوفة شبكات؛ `GET /network/{nwid}` (404 إن لم يكن منضماً)؛
- `POST/PUT /network/{nwid}` → **join** idempotent + تطبيق allow* اختيارية؛
- `DELETE /network/{nwid}` → **leave** → `{"result":true}`.
- حقول الشبكة (عقد الواجهة): id/nwid(16hex), mac (مشتق حتمي من nodeAddr+nwid — MAC.hpp:163), name, status ∈ {REQUESTING_CONFIGURATION, OK, ACCESS_DENIED, NOT_FOUND, PORT_ERROR, CLIENT_TOO_OLD, AUTHENTICATION_REQUIRED}, type PRIVATE/PUBLIC, mtu (2800), portDeviceName, allowManaged/Global/Default/DNS, assignedAddresses["10.x.y.z/24"], routes[{target,via,flags,metric}], dns{domain,servers[]}, netconfRevision, multicastSubscriptions.
- عند الإقلاع: إعادة انضمام تلقائية من `networks.d/*.conf`.

## 2. دورة حياة الانضمام (node/Network.cpp)
join → requestConfiguration → VERB_NETWORK_CONFIG_REQUEST إلى عنوان الـcontroller المضمّن في nwid → config → tap+IPs+routes (status OK). الفشل: NOT_FOUND/ACCESS_DENIED/AUTHENTICATION_REQUIRED. تحديث دوري للمراجعات.

## 3. تطبيق التكوين على OS (syncManagedStuff :3387)
- بوابات أمان: allowManaged للعناوين، allowDefault للمسار الافتراضي، allowGlobal لعناوين global، لا تطبيق أبداً على loopback/link-local/multicast.
- **المسار الافتراضي عبر shadow routes**: تفريق 0.0.0.0/0 إلى /1+/1 بدل استبداله (ManagedRoute.cpp:59-80) — يجب المحافظة على ذلك لأمان uplink.
- DNS على لينكس no-op في ZeroTier نفسه — التكامل لاحقاً مع systemd-resolved.
- MTU الافتراضي 2800 — فوق QUIC يحتاج قصة إعادة تجميع أو ضبط MTU (انظر §6.3).

## 4. بدائل COM (CertificateOfMembership) في نموذج zgalaxy-rs
COM يحل: إثبات تفويض عضو لعضو دون مرور controller مع نشر سحب التفويض خلال max-delta زمني. بما أن zgalaxy-rs يعتمد بنية controller/route مركزية مع QUIC:
**البديل المعتمد**: عضوية قصيرة TTL موقّعة من controller (nwid, memberId, netconfRevision, iat/exp) تُتحقق عند تأسيس اتصال QUIC مع العضوية؛ التغيير في netconfRevision يبطل القديمة؛ مسار سحب فوري برفض التوكن.

## 5. أساسيات LAN gaming لا يجوز إغفالها
1. **ARP يجب أن يعمل**: TAP(L2) → تمرير إطارات ARP؛ TUN(L3) → ARP-proxy في userspace (نمط osdep/Arp.cpp). NDP مثله لـIPv6.
2. البث مفعّل افتراضياً (اكتشاف سيرفرات اللعب) — ZeroTier يحوّل ARP broadcast إلى multicast انتقائي بADI؛ في نموذج relay: fan-out لكل الأعضاء يكفي للـlobbies الصغيرة.
3. **MTU**: لا يجوز القص الصامت؛ إما إعادة تجميع فوق QUIC datagrams أو MTU واقعي مع PMTU.
4. فلترة افتراضية **allow** لكل ethertypes (ARP/IPv4/IPv6) وإلا تنكسر الألعاب بصمت.
5. MAC حتمي لكل (node, nwid) — استقرار عبر إعادة التشغيل.
6. حفظ `networks.d/{nwid}.conf` + `.local.conf` لإعادة انضمام فورية عند الإقلاع.

## 6. من zerotier-go: قرارات النقل إلى QUIC (الأهم أولاً)

| آلية zerotier-go | القرار لـ zgalaxy-rs/QUIC |
|---|---|
| درع Salsa20/Poly1305, AES-GMAC-SIV, تفتيت/تجميع أسفل MTU, HELLO/OK | **DROP** — TLS1.3+QUIC يوفرها |
| إدارة المسارات: heartbeat 14s, انتهاء 243s, تقييم جودة, failover | **استبدال**: idle-timeout+keep-alive 5s، connection migration للمسار |
| WHOIS/بندق الطلبات/إعادة إرسال | **تقليص**: streams موثوقة + طابور frame صغير محدود أثناء التأسيس |
| Rendezvous عبر roots + punch | **P0 معدّل**: نفس الفكرة، Initial حقيقي بدل junk-bytes |
| PUSH_DIRECT_PATHS + مراقبة السطح الخارجي | **P1 نحيف**: تبادل مرشّحين عبر control stream ثم migrate |
| Bonding/تعدد مسارات | **مرحلة 2** (QUIC MTP غير جاهز): مسار واحد + migration |
| قواعد/COM/capabilities/tags | **P0**: مستقلة عن النقل — تُنقل كما هي (الشكل المعتمد: توكن عضوية §4) |
| تكوين الشبكة chunks+توقيع+نشر سريع | **P0**: يبقى overlay-protocol فوق streams |
| Multicast LIKE/GATHER + تعلم جسور MAC | **P0** للاستخدام اللعب (fan-out relay يبسّطها) |
| نمط "core واحد + callbacks مرتبة/متزامنة منفصلة" | **P0 معماري**: actor واحد + قنوات mpsc محدودة الحجم — مناسب مباشرة لـtokio |
| سياسة ذاكرة محدودة بكل شيء + LRU (peers 4096, جسور 65536, ...) | **P0**: نفس الحدود في Rust؛ حدّ قنوات quinn |
| TCP fallback | **P1**: نفق TCP يحمل QUIC أو MASQUE — لاحقاً |
| LZ4 | **P2**: قياس أولاً |
| ذاكرة endpoints للأقران + 0-RTT | **P1**: session tickets تكافئ 0-RTT |

**تنبيه توافق حاسم**: الانتقال إلى QUIC يقطع التوافق السلكي مع عقد ZeroTier الرسمية بالكامل (مقصود في الوثيقة: استبدال كامل، جذور ZGALAXY تتحدث QUIC).

## 7. هيكل وحدات مقترح (من كليهما)
core (actor الأقران/الاتصالات) | proto (رسائل فوق streams/datagrams) | identity | world | netcfg | rules+creds (membership tokens) | transport (quinn+سياسات migration+relay) | iplink (TUN/TAP, managed routes/ARP) | trace.
