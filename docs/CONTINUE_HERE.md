# استكمل من هنا — دليل الجلسة الجديدة (آخر تحديث: ختام جولة الواجهة، ما بعد 2026-08-23)

هذا أول ملف تقرؤه أي جلسة جديدة. كل شيء آخر متفرع منه.

## 0. خط الزمن الوظيفي (اقرأه ثانياً — الأهم لفهم تطور المشروع)
**`docs/TIMELINE.md`** — الخريطة الكاملة بالوظائف والمراحل بترتيبها المنطقي:
33 محطة عبر ثلاثة محاور (المحرك → تكامل ztnet → واجهة المستخدم)، كل محطة
بتفاصيلها ومكانها في الكود وطريقة تحققها، ثم الحالة الرقمية الختامية وخريطة
ما يلي. **إن قرأت ملفاً واحداً بعد هذا فهو ذاك.**

## 1. الوثائق بالترتيب
- **جديد 2026-10-10:** `docs/GTP-INTEGRATION.md` — تجريد النقل (MeshTransport) وخطة
  إضافة GTP؛ وقسم الفحص المعمّق أعلى `docs/PENDING_WORK.md`.
0. **`docs/TIMELINE.md`** — خط الزمن الوظيفي (جديد).
1. هذا الملف.
2. `docs/PENDING_WORK.md` — سجل كل المنجز/المتبقي بالتفصيل (محدّث).
3. `docs/PENDING_WORK.md` · `docs/ROLLOUT-QUIC-PRODUCTION.md` — البوابات والنشر.
4. `docs/ZTNET-INTEGRATION.md` — تشغيل ztnet فوق zgalaxy-rs كاملاً.
5. وثيقة متطلبات الواجهة المصدر:
   `/home/ggonlinux/zt/zgalaxy-rs/info/وثيقة_مواصفات_واجهة_عميل_شبكة_Windows_Rust_Slint.md`.

## 2. المواقع والوصول
- المستودعات (منسوخة ومحدثة): `~/zgalaxy-work/zgalaxy-rs` (الرئيسي) و`~/zgalaxy-work/ZGALAXY`،
  ومرجعان: `~/zgalaxy-work/ZeroTierOne` و`~/zgalaxy-work/zerotier-go`.
- GitHub: dreamzone-cc/zgalaxy-rs وdreamzone-cc/ZGALAXY (كل شيء مدفوع).
- سيرفر ZGALAXY: `ssh dz171@192.168.1.171` — سيرفر ztnet: `ssh dz161@192.168.1.161`
  (كلمة السر لـsudo نفسها لكل من dz161/dz171 والمضيف المحلي).
- على 161: حاويتا اختبار ب1 `zg-a`/`zg-b` + السكربت `/tmp/b1_test.sh` (v2 — يجتاز كاملاً) + الباينري musl `/tmp/zgrs2`.
- حاوية الإنتاج: `zerotier` (zgalaxy-rs باسم zerotier-one، خلفها ztnet + postgres على ztnet_app-network) —
  **النشر/التراجع فقط عبر `scripts/deploy_161.sh`** (نسخة على 161: `/tmp/deploy_161.sh`؛ modes: deploy/verify/rollback).
- رفع ملفات إلى 161: `python3 ~/zgalaxy-work/bin/sshput.py local user@host:remote` (pexpect+scp).

## 3. أين نقف بالضبط
آخر commit: `d3eb712` (zgalaxy-rs). 30 اختباراً أخضر، clippy نظيف.
- البوابة أ (أمان) مكتملة: توكن عضوية موقّع، ربط هوية معلنة، فرض allowManagementFrom.
- المرحلة 2 (data plane) مكتملة ومثبتة: TAP حقيقي L2 + MTU 1186 + MAC حتمي + تطبيق التكوين على المضيف.
- **ب1 مغلقة (2026-08-22)**: عقدتان حقيقيتان TAP↔TAP عبر QUIC في zg-a/zg-b —
  join → تسجيل عضو → تخويل REST → OK + IP تلقائي (10.244.0.10) → ping B→A ‏3/3 و0% فقد و~0.7ms
  (عبر nsenter — الصورة بلا ping). "مهمة sync الميتة" كانت وهماً تشخيصياً: السجل كان يتجاهل RUST_LOG
  (subscriber ثابت INFO) وفشل الإرسال debug فقط + نافذة السكربت 8s أقصر من أول دورة ناجحة (~15s).
  أُضيف: EnvFilter + heartbeat لكل دورة + spawn_watched (panic watcher).
- **ب5/ب6 مغلقة (2026-08-22)**: باينري musl (d3eb712) منشور على حاوية الإنتاج مع UDP افتراضياً.
  النسخ الاحتياطية: `/home/dz161/backups/{zerotier-one,ztnet_zerotier-volume}.20260822-120708.*`
  والتراجع: `sudo bash /tmp/deploy_161.sh rollback 20260822-120708`. VERIFY-PASS: الهوية محفوظة
  (ef313fb5c9)، 3 شبكات + 5 أعضاء محمّلة، ztnet بلا أخطاء.
- **ب3 مغلقة (2026-08-22، جلسة التدقيق المستقل)**: NodeAnnounce/Datagrams → PeerManager +
touch_member_last_seen (بثبات loopback)، RTT prober كل 10s عبر control streams يغذي /peer بزمن
مُقاس، NAT worker محصور بالوضع القديم. الدليل: B1-PASS بثنائية ب3 + /peer من الطرفين (نظير
فعلي، مسار حي، latency=1ms على الجسر المحلي). 32 اختباراً أخضر، clippy 0.
- **أ3 مغلقة (2026-08-22)**: مصافحة تحدي/إثبات إلزامية على كل اتصال QUIC —
  NodeChallenge → AnnounceProof موقّع ed25519 مرتبط بعنوان العقدة؛ الحضور والتسجيل
  لا يُمنحان إلا بعد إثبات صالح (رفض مبكر مؤكد حيّاً في سجل zg-a)، مع مصفوفة
  اختبارات سلبية كاملة. 33 اختباراً أخضر، clippy 0.
- **ب2 مغلقة (2026-08-22)**: وحدة l2_switch (تعلّم MAC→endpoint + evict 5m +
  6 اختبارات وحدة) وتوجيه انتقائي في مسار الإرسال — unicast للمالك فقط،
  flood للبث/المجهول. B1-PASS بثنائية ب2 (~0.8ms). 39 اختباراً أخضر، clippy 0.
- **ب4 مغلقة (2026-08-22)**: idle-timeout 15s + replace-on-dial (الميت لا يحتجز
  الفتحة) + إخلاء MAC عند الفقد + أهداف مثبتة أولاً بإرسال متوازٍ + نبضة تكيفية
  500ms/3s + انتظار AnnounceAccepted قبل طلب التكوين. الدليل: restart zg-b →
  **join→ping 2.1s (≤5s)** بهوية محفوظة. 40 اختباراً، clippy 0.
- **ج1+ج2 مغلقتان (2026-08-22، المحرك)**: nodeAddress في planet/info؛ هويات
  جذور حقيقية لكل عقدة في unified planet عبر /api/v1/identity/public العامة +
  التقاط تلقائي عند addNode + فشل صريح عند النقص. اختبار R4 صار واعياً بأدوات
  zgalaxy-rs (skip أنيق بلا أدوات)، وR2 يتحقق من هوية مولدة حقيقياً.
- **إصلاح عابر**: idtool validate أُضيف لـzgalaxy-rs (كان B1 بالمحرك يكسر مع
  الثنائية الجديدة) + rc=2 لسوء استخدام CLI عبر argv0.
- **أ1-in-QUIC مغلقة (2026-08-23)**: إصدار/تخزين/تقديم/تحقق توكن العضوية
  الموقعة يعمل دورة كاملة فوق QUIC (توكن حي ظاهر في /network، صفر رفضات
  خاطئة). **جميع شروط تفعيل transportMode=quic على الإنتاج مستوفاة.**
- **E1 مغلقة (2026-08-23، المحرك)**: /install.sh ينزّل ثنائية العميل فعلياً
  من مستودع zgalaxy-rs عند غيابها (كانت تطبع فقط) — 26 نجاح+1skip أنيق.
- **R1+R2 منفّذتان (2026-08-23)**: نشر `bcde6efe` (HEAD be62a4c) على حاوية الإنتاج
  VERIFY-PASS (تراجع: 20260823-151624)، ثم B1-PASS وB4-PASS ‏(2.1s) ونقع ‏31 دقيقة
  (8959/8959، فقد ‏0%، mdev ‏0.10ms، صفر إسقاطات). **D1 محسومة بالجرد**: عضو حي واحد
  فقط من ثلاثة → R3 محجوب حتى ترقية العضوين الآخرين أو وضع مزدوج QUIC+UDP.
  متبقٍ من R2: بوابة تخويل ztnet الرسمية (تفاعل بشري بالمتصفح).
- **مرحلة 3/P1 منفّذة (2026-08-23)**: في `zgalaxy-ui/` — Tray حقيقي بقائمة ديناميكية
  + نوافذ Advanced/Preferences/About + إعدادات محفوظة؛ بناء وclippy نظيفان واختبار
  دخان ضد REST وهمي ناجح. الفجوة الموثقة: تحديث أعلام allow* يحتاج نقطة نهاية
  daemon جديدة (مرحلة 4).
- **مرحلة 4 جزئياً مغلقة (2026-08-23)**: نقطة نهاية إعدادات الشبكة المحلية —
  `POST /network/{nwid}` يقبل جسماً اختيارياً بمفاتيح ‏ZeroTier ‏camelCase
  (allowManaged/allowGlobal/allowDefault/allowDNS؛ المفتاح الأخير بإعادة تسمية
  صريحة لأن camelCase ينتج allowDns) ويطبّق الجزئي دون مسّ الغائب، والحقول أصبحت
  حقيقية في `/network` (لم تعد مضمّنة ثابتة). **الفرض فعلي**: allowManaged يحكم
  تطبيق العناوين/المسارات على المضيف، وallowDefault يمنع مسار 0.0.0.0/0. واجهة
  Slint‏ (Advanced) أُوصلت بالمسار مع مانع وميض ‏3s. ‏43 اختباراً أخضر (+3) وclippy
  نظيف. **منشور على الإنتاج** (ثنائية `036153fea4aa`، تراجع ‏20260823-170948) مع
  تحقق حي من دورة الإعدادات كاملة على معرّف اختبار مؤقت ثم تنظيفه (سجل التنفيذ).
  متبقٍ يدوي: نقر فعلي على المربعات أمام شاشة (المسار مختبر بoneshot وعلى الإنتاج).
- **ختام 2026-08-23**: إغلاق عيب CLI الموثق (استبدال JSON الصامت) بدالة نقية
  parse_rest_body + 3 اختبارات انحدار، وإشعارات حالة الاتصال في zgalaxy-ui
  (notify-send، تحترم notify_status). ‏**46 اختباراً أخضر، clippy صفر** على
  المستودع كله. كل شيء غير ملتزم بانتظار أمر commit من المالك.
- **تشغيل فعلي + إصلاح جوهري (2026-08-23)**: الواجهة شُغّلت على سطح المكتب ضد
  الدايمون المحلي الحقيقي (العقدة 5b07896437، شبكة الإنتاج ef313fb5c9f817a2،
  "Ready to play" 10.121.15.7، أيقونة التراي حية). كشف التشغيل عيباً: تحديث
  خصائص Slint من خيط خلفي يُهمل — أعيد بناء خيط الاستقصاء على
  upgrade_in_event_loop بالكامل. القاعدة تكرست عملياً: لا اعتماد على اختبار
  دخان صامت لإثبات واجهة.
- **مواءمة الواجهة مع وثيقة المواصفات (2026-08-23 مساءً)**: اعتُمدت وثيقة
  Slint مرجعاً أساسياً؛ أعيدت هيكلة zgalaxy-ui إلى app/network/platform/storage
  (§11.2)، خمس حالات كاملة بتسميات gamer-first مشتقة مركزياً (§4.2/§10)،
  Join Dialog مستقل (§6.3)، CTA للفراغ (§14.1)، Retry (§14.4)، Copy عبر
  platform (§6.2)، وPalette للألوان (Dark mode، §8.4). clippy صفر و46 اختباراً.
  البقية المتبقية موثقة في PENDING_WORK (اختبارات Windows، صفحات Preferences
  المتعددة).
- **حزمة تكامل ztnet الرسمية (2026-08-23 مساءً)**: ‏`deploy/ztnet/` — Dockerfile
  (‏zgalaxy musl كـzerotier-one كامل الاستبدال) + docker-compose للتثبيت الجديد
  (postgres + zgalaxy + ztnet الرسمي غير المعدل عبر ZT_ADDR) + setup.sh
  (‏fresh/migrate/update-binary/verify/rollback). الوثيقة المفصلة:
  `docs/ZTNET-INTEGRATION.md` + قسم في README. **التحقق**: ‏verify عبر السكربت
  على إنتاج 161 = VERIFY-PASS (بما فيه مسار ztnet→المتحكم)؛ واختبار معزول
  محلي كامل لمنظومة جديدة من الصفر بصورة ztnet الرسمية 0.8.3 = المحرك healthy
  وztnet→zgalaxy ‏HTTP 200 (بعد إصلاح اقتباس write_local_conf إلى temp+docker cp).
- **إصلاح إنشاء الشبكات من ztnet (2026-08-23 مساءً)**: السبب الجذري كان
  `allowManagementFrom` داخل `settings` في local.conf الكلاسيكي لا يُستخلص
  (كما كان primaryPort) → ztnet مرفوض كلياً منذ بوابة أ دون انتباه. عولج في
  config.rs + 3 اختبارات، ونُشر `1a22b4a0` (تراجع 20260823-181253) وتحقق حي
  من داخل حاوية ztnet (‏/status وإنشاء شبكة = 200). 49 اختباراً أخضر.
- **الخطوة التالية المقترحة**: نشر آخر ثنائية على حاوية الإنتاج وتفعيل
  transportMode=quic عبر scripts/deploy_161.sh (deploy→verify، rollback جاهز)،
  ثم المرحلة 3: واجهة Slint.
  **الدليل التشغيلي الكامل**: `docs/ROLLOUT-QUIC-PRODUCTION.md` — مراحل R1/R2/R3
  بمعايير نجاح وأوامر تراجع ونقطة قرار توافق الأعضاء (D1) وسجل تنفيذ فارغ.
- **الخطوة التالية المقترحة**: تفعيل transportMode=quic تدريجياً على الإنتاج
  (الأبواب مغلقة؛ ابدأ بحاوية اختبار ثم zg-tool ثم الإنتاج مع مراقبة ztnet)، أو
  المرحلة 3: واجهة Slint.
- **الخطوة التالية المقترحة**: ج1 (unified planet identity في ZGALAXY engine — H1)
  أو تفعيل transportMode=quic تدريجياً على حاوية الإنتاج (الأبواب الأمنية أ1+أ3
  والبيانات ب1..ب4 كلها مغلقة الآن؛ يتبقى توكن العضوية الكامل داخل مسار QUIC).
- **الخطوة التالية المقترحة**: ب4 (التعافي/connection migration) أو ج1 (unified
  planet في المحرك) أو تفعيل transportMode=quic على الإنتاج (الأصبحت الشروط
  الأمنية أ1+أ3+ب1..ب3 مستوفاة — يتبقى توكن العضوية الكامل في مسار QUIC).
- **الخطوة التالية المقترحة**: ب2 (L2 learning/broadcast فوق QUIC) ثم ب4 (التعافي/
  connection migration)، أو ج1 (unified planet في المحرك).
- **الخطوة التالية المقترحة**: أ3 (ربط هوية QUIC: توقيع/تحدي قبل تفعيل transportMode=quic
إنتاجياً) ثم ب2 (L2 learning/broadcast) ثم ب4 (التعافي)، أو ج1 (unified planet في المحرك).
- **الخطوة التالية المقترحة** (حسب المسار الحرج في PRE_RESUME_PLAN): ب2 (L2 learning/broadcast) ثم
  ب3 (توصيل PeerManager/touch_member_last_seen في QUIC) وب4 (سلوك التعافي)، أو ج1 (unified planet
  في ZGALAXY engine). تفعيل transportMode=quic على حاوية الإنتاج يبقى معلقاً حتى توكن العضوية
  الكامل في مسار QUIC.

## 4. فخاخ بيئة معروفة (لا تضيع وقتاً فيها)
- rustup محلياً مكسور بسبب argv0: استخدم باينري التولشاين مباشرة:
  `TC=$(echo ~/.rustup/toolchains/stable-*/bin) && $TC/cargo ...`
- `pkill`/`pgrep` في هذه القشرة هي أدوات ZCode: استخدم `pgrep -f ... | xargs kill -9` أو fuser.
- باينري للنشر على صورة zgalaxy-zerotier:rs يجب أن يكون **musl**:
  `$TC/cargo build --release --target x86_64-unknown-linux-musl` (glibc المضيف أحدث من الصورة).
- محلياً المنفذ 9993 محجوز لzerotier-one حقيقي — اختبر على 9995 (`--endpoint`)،
  والـCLI يقرأ توكن `/var/lib/zerotier-one` أولاً محلياً → استخدم `--secret "$(cat ...)"`.
- ssh بكلمة سر عبر `python3 ~/zgalaxy-work/bin/sshrun.py user@host 'cmd'` (جاهز).
- sudo للـTAP محلياً متاح بنفس كلمة السر.
- مجلد عمل الـdaemon يحدَّد بـ`ZGALAXY_HOME` (بدونه يفضّل /var/lib/zerotier-one عند الكتابة).
- docker على 161 يحتاج sudo (`echo PASS | sudo -S docker ...`).
- اقتباس JSON عبر ssh+docker exec متداخل يحوّل `'{"a":b}'` إلى `{a:b}` — استخدم اقتباساً أحادياً
   داخلياً دائماً، وتذكر: CLI ‏`rest` **كان** يستبدل الجسم غير الصالح بـ`{}` بصمت — أُصلح بخطأ
   صريح مع اختبارات انحدار (2026-08-23)، لكن الاقتباس المتداخل يظل مصدر تشويه مستقلاً عن ذلك.

## 5. قواعد ثابتة
لا تعديل على ztnet إطلاقاً · QUIC هو النقل (لا UDP خام) · لا نقل أعمى من zerotier-go ·
لا منطق شبكة داخل Slint · لا وظيفة تُعد مكتملة قبل اختبارها وتوثيقها · لا اعتماد على اختبار واحد.
