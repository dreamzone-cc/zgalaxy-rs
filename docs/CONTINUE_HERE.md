# استكمل من هنا — دليل الجلسة الجديدة (آخر تحديث: 2026-08-22)

هذا أول ملف تقرؤه أي جلسة جديدة. كل شيء آخر متفرع منه.

## 1. الوثائق بالترتيب
1. هذا الملف.
2. `docs/PENDING_WORK.md` — سجل كل المنجز/المتبقي بالتفصيل (محدّث).
3. `docs/PRE_RESUME_PLAN.md` — بوابات ما قبل استئناف مراحل وثيقة المتطلبات.
4. `docs/EXECUTION_PLAN.md` + `docs/REFERENCE_ANALYSIS.md` — الخطة والتحليل المرجعي.
5. وثيقة المتطلبات المصدر: `/home/ggonlinux/zt/zgalaxy-rs/info/zgalaxy-rs_project_requirements_ar.md`
   وواجهة Slint: `وثيقة_مواصفات_واجهة_عميل_شبكة_Windows_Rust_Slint.md` (نفس المجلد).

## 2. المواقع والوصول
- المستودعات (منسوخة ومحدثة): `~/zgalaxy-work/zgalaxy-rs` (الرئيسي) و`~/zgalaxy-work/ZGALAXY`،
  ومرجعان: `~/zgalaxy-work/ZeroTierOne` و`~/zgalaxy-work/zerotier-go`.
- GitHub: dreamzone-cc/zgalaxy-rs وdreamzone-cc/ZGALAXY (كل شيء مدفوع).
- سيرفر ZGALAXY: `ssh dz171@192.168.1.171` — سيرفر ztnet: `ssh dz161@192.168.1.161`
  (كلمة السر لـsudo نفسها لكل من dz161/dz171 والمضيف المحلي).
- على 161: حاويتا اختبار ب1 `zg-a`/`zg-b` + السكربت `/tmp/b1_test.sh` + الباينري musl `/tmp/zgrs2`.
  حاوية الإنتاج: `zerotier` (zgalaxy-rs مُسمّى zerotier-one، خلفها ztnet + postgres) — **لا تُمسّ إلا وفق خطة النشر ب5/ب6**.

## 3. أين نقف بالضبط
آخر commits: `69ab158` (zgalaxy-rs) و`e85c9da` (ZGALAXY). 30 اختباراً أخضر، clippy نظيف.
- البوابة أ (أمان) مكتملة: توكن عضوية موقّع، ربط هوية معلنة، فرض allowManagementFrom.
- المرحلة 2 (data plane) مكتملة: TAP حقيقي L2 + MTU 1186 + MAC حتمي + تطبيق التكوين على المضيف،
  مثبتة سلوكياً بـroot محلياً (loopback AF_PACKET ناجح، الواجهة تظهر في ip link).
- **النقطة المتوقفة (ب1 على 161)**: مهمة sync loop في الـdaemon الحي لا تنفّذ أي محاولة اتصال
  (سجل B ساكن تماماً بعد الإقلاع حتى مع RUST_LOG=info,zgalaxy_rs=debug، رغم أن اختبار QUIC
  المفرد ينجح). قبل التوقف أُصلح: حل أسماء ZGALAXY_EXTRA_ENDPOINTS عبر DNS، ومهلة 5s لكل هدف.
  الاشتباه: panic صامت يقتل المهمة أو عدم وصولها للحلقة.

### خطوة الاستئناف الفورية (ب1)
1. أضف heartbeat: `info!` في بداية كل دورة sync (بعد interval.tick) يطبع عدد الشبكات والأهداف —
   يثبت فوراً هل المهمة حية.
2. إن كانت حية: ارفع مستوى تسجيل send_control/connect إلى info مؤقتاً وشغّل `/tmp/b1_test.sh`.
3. إن كانت ميتة: غلّف جسم المهمة بـ`std::panic::catch_unwind` أو استخدم
   `tokio::spawn` مع معالج panics (JoinHandle خطأ) واكتب panic message للسجل.
4. بعد نجاح التسجيل: استكمل ب1 (تخويل العضو عبر REST كما في السكربت → انتظار OK+IP → ping من
   داخل شبكة الحاوية أو ثبّت iputils-ping في حاوية B).
5. ثم ب5/ب6: نشر musl binary على حاوية الإنتاج (نسخة احتياطية أولاً — نفس خطوات الجلسة السابقة
   الموثقة في git log commit a15bb72 وما قبله) + فحوص ztnet (status/controller/network/member).

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

## 5. قواعد ثابتة
لا تعديل على ztnet إطلاقاً · QUIC هو النقل (لا UDP خام) · لا نقل أعمى من zerotier-go ·
لا منطق شبكة داخل Slint · لا وظيفة تُعد مكتملة قبل اختبارها وتوثيقها · لا اعتماد على اختبار واحد.
