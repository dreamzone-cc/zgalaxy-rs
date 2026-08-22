# خطة تنفيذ ZGALAXY-rs — QUIC Transport + Slint Desktop + تكامل ZTNET
(مبنية على: info/zgalaxy-rs_project_requirements_ar.md + وثيقة مواصفات واجهة Slint)

## الحالة الحالية (نتائج التدقيق السابق 2026-08-21)
- zgalaxy-rs v1.3.0: REST controller متوافق مع ztnet ويعمل على 161 (منشور ويعمل).
- النقل الحالي: UDP خام بدون تشفير/MAC — **مخالف لقيد QUIC الإلزامي**.
- TUN وهمي (لا يقرأ/يكتب إطارات حقيقية) — لا يوجد data plane فعلي.
- لا توجد واجهة سطح مكتب.

## المراحل

### المرحلة 0 — جرد وتحليل مرجعي (توازي)
- [x] جرد zgalaxy-rs الحالي (تم في التدقيق السابق).
- [ ] تحليل zerotier-go (MetaCubeX) بعمق: peer/path/routing/معالجة حزم/ذاكرة/تزامن.
- [ ] تحليل ZeroTierOne المرجعي: الآليات التي يجب إعادة تصميمها لـ QUIC.
- [ ] مصفوفة مقارنة ZeroTierOne / zerotier-go / zgalaxy-rs (docs/COMPARISON.md).

### المرحلة 1 — QUIC Transport Core (حرج)
- [ ] إضافة طبقة نقل QUIC (quinn + rustls) كوحدة src/quic.rs مستقلة.
- [ ] نقطتا استعمال: QUIC Streams (تحكم/تكوين شبكة، موثوق) + QUIC Datagrams (إطارات بيانات، RFC 9221) — القرار مبني على طبيعة البيانات لا على التعميم.
- [ ] هوية العقدة: شهادة ذاتية التوقيع مشتقة من identity، تحقق على مستوى التطبيق (ربط شهادة↔node address).
- [ ] جعل QUIC هو النقل الافتراضي للـ daemon (transport = "quic") مع الإبقاء على مسار UDP القديم مؤقتاً خلف راية حتى اكتمال الترحيل.
- [ ] اختبارات integration محلية: عقدتان تقيمان جلسة QUIC وتبادلا Frame عبر Datagram وNetworkConfig عبر Stream.

### المرحلة 2 — Data Plane حقيقي
- [ ] TUN حقيقي على لينكس (tun crate + tokio async) قراءة/كتابة إطارات فعلية.
- [ ] ربط TUN ↔ QUIC datagrams عبر قنوات مع backpressure (bounded channels).
- [ ] توجيه L3 (routes) عبر route_manager إلى واجهة النظام.

### المرحلة 3 — واجهة Slint (zgalaxy-ui crate جديدة)
- [ ] معمارية الوثيقة: Core (state/commands/service client) + Slint Windows + Tray (SystemTrayIcon) + platform/{windows,linux}.
- [ ] P0: Tray + Join Network + قائمة الشبكات + Disconnect + الحالات (Connected/Connecting/Waiting/Disconnected/Error).
- [ ] الاتصال بالـ daemon عبر local REST API فقط (لا منطق شبكة في UI).
- [ ] Linux أولاً للبناء والاختبار، Windows cross-compile لاحقاً (الوثيقة Windows-first للتجربة النهائية).

### المرحلة 4 — API الـ daemon المطلوبة للواجهة
- [ ] GET /network (الشبكات المنضم إليها + حالتها) — إن لم تكن موجودة.
- [ ] POST /network/{nwid} (join) / DELETE (leave) — مطابقة لدلالات ZeroTier المحلية التي تتوقعها الواجهة.
- [ ] حماية: الالتزام بـ allowManagementFrom من local.conf (سد الثغرة السابقة).

### المرحلة 5 — تكامل ztnet (بدون تعديله)
- [x] controller REST متوافق ومختبر على 161 (تم).
- [ ] التحقق من دورة حياة كاملة من الواجهة الجديدة: join → authorization عبر ztnet → جاهزية.

### المرحلة 6 — اختبارات وقياس
- [ ] Unit + integration (loss/latency/reconnect/high-load) + benchmarks (قبل/بعد كل تحسين).
- [ ] قياس: زمن إنشاء الاتصال، throughput، CPU/ذاكرة idle.

### المرحلة 7 — توثيق
- [ ] docs/QUIC_DESIGN.md (قرارات Streams vs Datagrams، مصادقة الهوية عبر QUIC).
- [ ] تحديث ARCHITECTURE.md وREPORT.md.

## قواعد إلزامية (من الوثائق)
- لا تعديل على ztnet. لا استبدال QUIC بـ UDP خام. لا نقل أعمى من zerotier-go.
- لا منطق شبكة داخل Slint. لا اعتماد على اختبار واحد. لا اعتبار وظيفة مكتملة قبل اختبارها وتوثيقها.

## تنفيذ هذه الجلسة (الأولوية)
1. كتابة هذه الخطة (تم).
2. تحليل zerotier-go + ZeroTierOne عبر وكلاء متوازيين → COMPARISON.md.
3. تنفيذ المرحلة 1 (QUIC core) مع اختباراتها + commit/push.
4. تنفيذ المرحلة 4 (نقاط API للواجهة) إذا لزم.
5. هيكل zgalaxy-ui بـ Slint (P0) إن اتسع الوقت.
