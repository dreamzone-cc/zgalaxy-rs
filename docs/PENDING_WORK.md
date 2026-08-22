<!-- اقرأ docs/CONTINUE_HERE.md أولاً — دليل الاستئناف المركزي -->
# عناصر لم تُنجز بعد — سجل الحالة (2026-08-22)

حُفظت هنا قبل الانتقال للمرحلة التالية. التحديث يتم عند إنجاز أي بند.

## zgalaxy-rs — Data Plane (المرحلة 2) — منفذة 2026-08-22:
- [x] TAP حقيقي L2 على لينكس (/dev/net/tun عبر crate tun): قراءة/كتابة Ethernet فعلية + خفض رحيم headless بلا CAP_NET_ADMIN (الـcontroller/ztnet لا يتأثر).
- [x] التحقق السلوكي بامتيازات فعلية: اختبار loopback ثنائي الاتجاه بمقبس AF_PACKET (host→mesh وmesh→host) ناجح، والـdaemon الحي يُظهر zgalaxy0 (TAP, MTU 1186, UP+LOWER_UP) في ip link.
- [x] MTU: 1186 في وضع QUIC (1200 - 14 بايت رأس Ethernet) — بلا إسقاط صامت للإطارات الكبيرة.
- [x] MAC حتمي لكل (node address, nwid) عبر SHA-256 — مستقر عبر إعادة التشغيل ومختلف بين العقد (اختبار وحدة).
- [x] join لم يعد يخترع عناوين/مسارات: REQUESTING_CONFIGURATION حتى رد الـcontroller.
- [x] تطبيق عنوان IP + MAC + المسارات المُدارة على الواجهة عند وصول NetworkConfigResponse (مرة لكل شبكة).
- [x] ZGALAXY_HOME env لتحديد مجلد العمل صراحةً.
### ب1 (عقدتان على 161) — **مكتملة 2026-08-22 (B1-PASS ×2 متتاليتين)**:
- [x] بنية الاختبار جاهزة وقابلة للتكرار: /tmp/b1_test.sh على 161 (حاويتا zg-a/zg-b من صورة zgalaxy-zerotier:rs + باينري musl ثابت /tmp/zgrs2 — glibc المضيف 2.39 أحدث من الصورة، فالباينري يجب أن يكون musl). النسخة الحالية من السكربت (v2): pool يبدأ من .10 (لأن .1 ثابت للـcontroller — pool من .1 يعطي تعارضاً)، polling بدل نوافذ ثابتة، تحقق من استجابة التخويل، وping عبر `nsenter -t <pid> -n ping` من مضيف docker (الصورة بلا ping).
- [x] **حُلّت: "مهمة sync loop لا تنفّذ اتصالاً" كانت وهماً تشخيصياً لا عطلاً** — السبب: الـsubscriber في main.rs كان يتجاهل RUST_LOG (hardcoded INFO) وكل فشل إرسال يُسجَّل debug فقط، والسكربت كان يجلب الأعضاء بعد 8s بينما أول دورة ناجحة تحتاج ~15s (مهلة الهدف الميت الافتراضي 5s + handshake + الدورة التالية). أُصلح: EnvFilter يحترم RUST_LOG، heartbeat لكل دورة (شبكات/أهداف + sent/ok/failed)، وspawn_watched يكتب panic أي مهمة خلفية للسجل.
- [x] الدورة الكاملة تعمل: join → طلب تكوين QUIC → تسجيل عضو عند الـcontroller → تخويل عبر REST (بنفس واجهة ztnet) → OK + تعيين IP تلقائي (10.244.0.10) وتطبيقه على zgalaxy0 → ping B→A (10.244.0.1) عبر TAP↔QUIC: 3/3 حزم، 0% فقد، ~0.7ms (يثبت ARP/البث L2 أيضاً لأن ping عبر /24 يتطلب ARP أولاً).
- [x] ping نهائي B→A — عبر nsenter من مضيف docker داخل network namespace الحاوية (لا iputils-ping في الصورة).

- [x] اختبار كامل بين عقدتين حقيقيتين عبر QUIC (TAP↔TAP) على السيرفرات — ب1 أعلاه (2026-08-22).
- [x] ARP traffic بين عقدتين — مثبت سلوكياً ضمن ب1 (نجاح ping يستلزم ARP resolution عبر البث في الشبكة الافتراضية).

## zgalaxy-rs — ب4 مكتملة 2026-08-22 (التعافي وإعادة الاتصال):
- [x] مهلة خمول قصوى 15s على اتصالات QUIC (كلا الطرفين) + keep-alive 5s → كشف فقد
      النظير محدد الزمن بدل الافتراضي (~40s).
- [x] استبدال فوري عند طلب اتصال جديد ناجح: الاتصال الميت لم يعد يحتجز الفتحة
      (كان السبب الجذري لتعافٍ ~40s)؛ القديم يُغلق برمجياً وحارس stable_id يمنع
      تنظيفاً خاطئاً وإخلاء MACs لاتصال حي.
- [x] إخلاء MACs المتعلمة فور Disconnected الحقيقي → الرجوع للـflood حتى العودة.
- [x] ترتيب الأهداف: المثبتة (المشغل) أولاً ثم جذور المحلل، مع إزالة تكرار.
- [x] إرسال متوازٍ لكل الأهداف (الهدف الميت لا يعطّل البقية — كان تسلسلياً 5s/هدف).
- [x] نبضة تكيفية: 500ms ما دامت هناك شبكة غير OK، ثم 3s استقراراً.
- [x] تسلسل أ3↔ب4: انتظار AnnounceAccepted (سقف 750ms) قبل طلب التكوين — الطلب
      لا يضيع في سباق الإثبات (كان يتطلب دورة كاملة إضافية).
- [x] **الدليل الحي**: B1-PASS أساس، ثم docker restart zg-b (الهوية محفوظة) →
      **join→ping ناجح خلال 2.1s** (معيار القبول ≤5s) ✅ — 40 اختباراً أخضر، clippy 0.

## zgalaxy-rs — ب2 مكتملة 2026-08-22 (L2 learning/broadcast):
- [x] وحدة l2_switch.rs: جدول تعلّم MAC→endpoint (تعلّم رجعي من إطار وارد، أحدث
      endpoint يفوز عند التجوال، evict بعد صمت 5 دقائق عبر مسبار RTT الدوري).
- [x] توجيه انتقائي صادر: unicast للمالك المُتعلم فقط؛ broadcast/multicast/مجهول →
      flood لكل الأقران المتصلين. جدول واحد مشترك بين TUN/QUIC-events/relay/prober.
- [x] 6 اختبارات وحدة لجدول التعلم (تعلم/حل/تجوال/إخلاء/تحليل Ethernet/كشف bcast-mcast).
- [x] الدليل الحي: B1-PASS بثنائية ب2 (ping 3/3، 0% فقد، ~0.8ms — يشمل ARP-broadcast
      ثم ARP-reply وICMP unicast عبر الجدول) + /peer حي.
- ملاحظة: التفريق بين unicast/flood خارجياً يحتاج عقدة ثالثة مراقبة — السلوك مغطى
  باختبارات الوحدة والمنطق؛ اختبار 3 عقد مقترح ضمن E2E لاحقاً.

## zgalaxy-rs — ب3 مكتملة 2026-08-22 (تحقق حي على 161):
- [x] NodeAnnounce → PeerManager + touch_member_last_seen (بثبات loopback-guard).
- [x] Datagram presence refresh مُقيَّد (5s/نظير) — lastContact حي في /peer.
- [x] RTT حقيقي: prober كل 10s عبر control streams → Pong يقيس عند المرسل → latency في /peer (1ms على جسر docker المحلي).
- [x] NAT raw-UDP worker يعمل فقط في الوضع القديم (QUIC keep-alive مدمج 5s + probes تؤدي نفس الدور).
- [x] الدليل: B1-PASS بثنائية ب3 + /peer من الطرفين يعرضان النظير الفعلي بمسار حي وlatency مُقاسة.

## zgalaxy-rs — أمان/بروتوكول (المرحلة 1 المتبقي)
- [ ] ربط شهادة QUIC بعنوان العقدة (pinning) — الحالي SkipServerVerification + NodeAnnounce بدون تحقق إجباري.
- [ ] توكن عضوية موقّع من controller قصير TTL (بديل COM) يُتحقق عند تأسيس جلسة QUIC.
- [ ] register_join_request في مسار QUIC يُمرَّر له member id غير صالح حالياً (IP بدل 10-hex) — يحتاج ربط عنوان العقدة المعلن في NodeAnnounce.
- [ ] NAT traversal/rendezvous فوق QUIC (مقترح التصميم في REFERENCE_ANALYSIS.md §6).
- [ ] connection migration عند تغيّر المسار.

## zgalaxy-ui (المرحلة 3)
- [ ] SystemTrayIcon حقيقي + قائمة ديناميكية (يتطلب إصدار Slint الداعم للـtray).
- [ ] نوافذ: Networks، Preferences، About، Advanced Network Details.
- [ ] Windows-first: بناء واختبار على Windows 10/11 (cross-compile أو CI).
- [ ] تفعيل allowManaged/allowGlobal/allowDefault/allowDNS في POST /network/{nwid}.

## ZGALAXY (engine) — نتائج فحص 2026-08-22 (المُصلَّح منها في هذا الـcommit):
- [x] FK constraint failed عند استيراد sessions.json — السبب الجذري: معاملة all-or-nothing + INSERT OR IGNORE لا يغطي FK + جلسات يتيمة. أُصلح (تخطي per-row + seed admin عند فشل الاستيراد).
- [x] /api/v1/metrics كان عاماً بدون مصادقة — أُصلح.
- [x] systemd unit في install.sh بدون WorkingDirectory (zgalaxy-rs يقرأ ./config نسبةً إلى CWD) — أُصلح: /var/lib/zerotier-one.
### متبقٍ (مرتب حسب الخطورة):
- [ ] H1: بناء unified planet يعطي كل الجذور نفس identity (clusterService/planetService:345-354، ClusterNode.identityPublic لا يُملأ أبداً) — cluster HA معطوب وظيفياً.
- [ ] H2: localInfo.nodeAddress غير موجود أبداً في getPlanetInfo — أضفه (حلل identity.public).
- [ ] M2: /api/docs عام — خلف requireRole أو DOCS_PUBLIC=0.
- [ ] M3: مزودو DDNS (DUCK_DNS/NO_IP/CUSTOM_WEBHOOK) مقبولون في الإعداد لكنهم no-ops — ارفضهم صراحةً.
- [ ] M4: install.sh — رفض IPv6 literals في Host regex، TRUST_PROXY/X-Forwarded-Proto.
- [ ] M5: فحص صحة cluster يستخدم منفذ الـengine المحلي للبعيد — أضف apiPort لكل عقدة.
- [ ] M6: planet وmoon يتشاركان moon.json واحد (الكتابة تتطابق) — فصل أو توثيق كقرار.
- [ ] L2: كاش الجلسات لا يعيد فحص SESSION_TTL في cacheGet.
- [ ] L3: /ready عام يقرأ 5 ملفات لكل استدعاء — كاش 5-10 ثوان.
- [ ] L5: نقاط قراءة بدون requireRole (federation/peers، identity/verify، cloudflare/*، ddns/status).
- [ ] L6: مقارنة توكن federation ليست constant-time.
- [ ] L8: entrypoint.sh يقرأ zerotier-one.port دون fallback.
- [ ] L10: cluster syncSecret مولَّد ولا يُستخدم.
### zgalaxy-rs — متبقٍ من فحص هذه الجلسة:
- [ ] P2: قناة TUN ما زالت Vec<u8> بدل Bytes (نسخة لكل إطار داخل).
- [ ] P4: حلقة المزامنة 3s×شبكات×أهداف — تقليل بعد اكتمال C2 (تم إصلاح الرد) بحسب الحاجة.
- [ ] P5: next_free_ip مسح خطي من بداية الـpool (سيء للـpools الواسعة) — bitmap/free-set.
- [ ] تنظيف: control::decode غير مستخدم، target_endpoint في main.rs، #[allow(dead_code)] قديم في transport.rs.
- [ ] Dockerfile: alpine 3.14 قديم + بناء ZeroTier من main غير مثبّت — يحتاج إعادة كتابة عند الانتقال إلى حاويات.
- [ ] استبدال zerotier-idtool/mkmoonworld الثنائيات المرفقة بتوليد native عبر zgalaxy-rs (world.rs يملك تنسيقه الخاص — توحيد التنسيق).
- [ ] حذف مراجع zerotier-cli join في web-console واستبدالها بأوامر zgalaxy-cli.
- [ ] تقييد REST API الخاصة بـ zgalaxy-rs بـ allowManagementFrom من local.conf (الكود يربط 0.0.0.0 بلا فرض القائمة).

## نشر/تكامل
- [x] **ب5/ب6 منجزة 2026-08-22**: نشر باينري المرحلة الحالية (musl, commit d3eb712) على حاوية الإنتاج `zerotier` في 161 مع UDP افتراضياً (local.conf بلا transportMode). النسخ الاحتياطية: `/home/dz161/backups/zerotier-one.20260822-120708.bak` + `ztnet_zerotier-volume.20260822-120708.tgz`؛ التراجع بأمر واحد: `bash /tmp/deploy_161.sh rollback 20260822-120708`. فحوص ما بعد النشر (سكربت `scripts/deploy_161.sh` في المستودع — deploy/verify/rollback): هوية العقدة محفوظة (ef313fb5c9)، الشبكات الثلاث والأعضاء الخمسة محمّلة، /status و/controller/network وmember و/network تجيب، TAP حقيقي أُنشئ، ztnet يعمل بلا أخطاء.
- [ ] تفعيل transportMode=quic على 161 بعد اكتمال data plane + توكن العضوية، مع اختبار دورة كاملة من الواجهة (join→تخويل عبر ztnet→جاهزية).
- [ ] Benchmarks: زمن إنشاء اتصال QUIC، throughput، CPU/ذاكرة idle، مقارنة قبل/بعد كل تحسين.
- [ ] اختبارات فقدان حزم/تأخير/إعادة اتصال/high-load طويلة المدى.

## ملاحظات اكتُشفت في جلسة 2026-08-22 (غير حرجة)
- CLI ‏`rest`: جسم JSON غير صالح يُستبدل بصمت بـ`{}` (`unwrap_or_else(|_| json!({}))` في cli.rs) — أخرج خطأ صريحاً بدلاً من ذلك (أضاع وقتاً في تشخيص التخويل عندما شوّهت الاقتباسات المتداخلة الجسم).
- ‏`/network` يعرض mtu من سجل الشبكة (2800 افتراضياً عند إنشاء شبكة بلا mtu صريح) بينما الواجهة الفعلية 1186 في وضع QUIC — اتساق العرض فقط.
- pool التعيين يبدأ من ipRangeStart كما هو — عند إعطاء الـcontroller عنواناً ثابتاً ضمن الـpool (مثل .1 في ب1) ابدأ الـpool بعده (عولج في السكربت؛ سلوك ZeroTier الأصلي مماثل).

## قواعد ثابتة
- لا تعديل على ztnet إطلاقاً. QUIC هو النقل. لا نقل أعمى من zerotier-go. لا منطق شبكة داخل الواجهة.
