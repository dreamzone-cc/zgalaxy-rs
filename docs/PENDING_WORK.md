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
### ب1 (عقدتان على 161) — حالة الجلسة 2026-08-22:
- [x] بنية الاختبار جاهزة وقابلة للتكرار: /tmp/b1_test.sh على 161 (حاويتا zg-a/zg-b من صورة zgalaxy-zerotier:rs + باينري musl ثابت /tmp/zgrs2 — glibc المضيف 2.39 أحدث من الصورة، فالباينري يجب أن يكون musl).
- [x] يعمل: إنشاء شبكة عبر CLI الجديد `rest`، join من B (الحالة REQUESTING_CONFIGURATION بصدق بلا عناوين مختلقة)، TAP فعلي بكلا الطرفين، عنوان يدوي للـcontroller على A (10.244.0.1).
- [ ] **معطّل — مهمة sync loop في daemon الحي لا تنفّذ أي محاولة اتصال** (سجل B ساكن تماماً بعد الإقلاع رغم RUST_LOG=zgalaxy_rs=debug؛ اختبارات QUIC المفردة تمر). أُضيف: حل أسماء المضيفين في ZGALAXY_EXTRA_ENDPOINTS (كان parse SocketAddr يرفض zg-a:9993 صامتاً) + مهلة 5s لكل هدف — لم يكشف بعد السبب. **الخطوة التالية الفورية**: heartbeat log في بداية كل دورة + فحص liveness المهمة (ربما panic صامت يقتلها) ثم إعادة ب1.
- [ ] ping نهائي B→A (الصورة بلا ping — استخدم docker exec من مضيف docker بشبكة الحاوية أو ثبّت iputils-ping).

- [ ] L2 learning/broadcast الصحيح (الحالي: إرسال كل إطار صادر لكل الأقران المتصلين — يكفي للـlobbies الصغيرة).
- [ ] اختبار كامل بين عقدتين حقيقيتين عبر QUIC (TAP↔TAP) على السيرفرات.
- [ ] ARP traffic بين عقدتين (يحتاج البند أعلاه + عقدتين).

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
- [ ] H2: PeerManager وtouch_member_last_seen غير موصولين في وضع QUIC (و/peer فارغ)؛ NAT worker يعمل بلا عمل في QUIC.
- [ ] P2: قناة TUN ما زالت Vec<u8> بدل Bytes (نسخة لكل إطار داخل).
- [ ] P4: حلقة المزامنة 3s×شبكات×أهداف — تقليل بعد اكتمال C2 (تم إصلاح الرد) بحسب الحاجة.
- [ ] P5: next_free_ip مسح خطي من بداية الـpool (سيء للـpools الواسعة) — bitmap/free-set.
- [ ] تنظيف: control::decode غير مستخدم، target_endpoint في main.rs، #[allow(dead_code)] قديم في transport.rs.
- [ ] Dockerfile: alpine 3.14 قديم + بناء ZeroTier من main غير مثبّت — يحتاج إعادة كتابة عند الانتقال إلى حاويات.
- [ ] استبدال zerotier-idtool/mkmoonworld الثنائيات المرفقة بتوليد native عبر zgalaxy-rs (world.rs يملك تنسيقه الخاص — توحيد التنسيق).
- [ ] حذف مراجع zerotier-cli join في web-console واستبدالها بأوامر zgalaxy-cli.
- [ ] تقييد REST API الخاصة بـ zgalaxy-rs بـ allowManagementFrom من local.conf (الكود يربط 0.0.0.0 بلا فرض القائمة).

## نشر/تكامل
- [ ] تفعيل transportMode=quic على 161 بعد اكتمال data plane + توكن العضوية، مع اختبار دورة كاملة من الواجهة (join→تخويل عبر ztnet→جاهزية).
- [ ] Benchmarks: زمن إنشاء اتصال QUIC، throughput، CPU/ذاكرة idle، مقارنة قبل/بعد كل تحسين.
- [ ] اختبارات فقدان حزم/تأخير/إعادة اتصال/high-load طويلة المدى.

## قواعد ثابتة
- لا تعديل على ztnet إطلاقاً. QUIC هو النقل. لا نقل أعمى من zerotier-go. لا منطق شبكة داخل الواجهة.
