# عناصر لم تُنجز بعد — سجل الحالة (2026-08-22)

حُفظت هنا قبل الانتقال للمرحلة التالية. التحديث يتم عند إنجاز أي بند.

## zgalaxy-rs — Data Plane (المرحلة 2 من EXECUTION_PLAN.md)
- [ ] TUN حقيقي على لينكس: فتح /dev/net/tun، قراءة/كتابة إطارات فعلية عبر tokio (الحالي: tun.rs وهمي — يهمل الإطارات الداخلة ولا يلتقط الصادرة).
- [ ] توجيه L3 عبر route_manager إلى واجهة النظام (الكود موجود لكن لا مستدعي).
- [ ] قصة MTU فوق QUIC: إما إعادة تجميع للإطارات > 1200 بايت فوق datagrams أو ضبط MTU للواجهة (ZeroTier يستخدم 2800) — دون قص صامت.
- [ ] MAC حتمي لكل (node address, nwid) بدل الاعتماد على nwid فقط (network.rs derive_mac).
- [ ] L2 learning/broadcast الصحيح (الحالي: إرسال كل إطار صادر لكل الأقران المتصلين).

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

## ZGALAXY (engine)
- [ ] خطأ إقلاع: "[ZGALAXY SQLITE] Failed to import sessions.json: FOREIGN KEY constraint failed" (ملاحظ على 171) — يُحقَّق في هذه الجلسة.
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
