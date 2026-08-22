# استكمل من هنا — دليل الجلسة الجديدة (آخر تحديث: 2026-08-22، بعد إغلاق ب1/ب5/ب6)

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
  داخلياً دائماً، وتذكر: CLI ‏`rest` يستبدل جسم JSON غير الصالح بـ`{}` **بصمت** (خلل موثق في PENDING_WORK).

## 5. قواعد ثابتة
لا تعديل على ztnet إطلاقاً · QUIC هو النقل (لا UDP خام) · لا نقل أعمى من zerotier-go ·
لا منطق شبكة داخل Slint · لا وظيفة تُعد مكتملة قبل اختبارها وتوثيقها · لا اعتماد على اختبار واحد.
