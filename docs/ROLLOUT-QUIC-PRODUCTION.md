# Runbook — تفعيل QUIC على الإنتاج تدريجياً + مسار المرحلة 3 (Slint)

> **الحالة:** كل الشروط المسبقة مستوفاة (أ1 توكن العضوية داخل QUIC، أ3 ربط
> الهوية، ب1 عقدتان E2E، ب2 L2 learning، ب3 حضور+RTT، ب4 تعافٍ 2.1s).
> **هذا الدليل تشغيلي حساس**: يمس حاوية إنتاج يمر عبرها أعضاء حقيقيون.
> **لا تُنفَّذ أي مرحلة دون إشارة صريحة من مالك المشروع.**
>
> آخر تحديث: 2026-08-23 · الثنائية المستهدفة: musl من `d662bfc` أو أحدث.

---

## R0 — قائمة ما قبل التنفيذ (كلها ✅ حالياً)

| الشرط | الحالة |
|---|---|
| توكن عضوية موقّع يُصدر ويُفرض في مسار QUIC | ✅ d662bfc (توكن حي ظاهر في /network على zg-b) |
| ربط هوية QUIC (تحدي/إثبات) إلزامي | ✅ ea608be |
| عقدتان TAP↔TAP عبر QUIC (join→تخويل→ping) | ✅ B1-PASS متكرر على zg-a/zg-b |
| L2 learning/broadcast | ✅ 3cf9524 |
| حضور + RTT حقيقي في /peer | ✅ e293419 |
| تعافٍ ≤5s بعد restart بتغيير مسار | ✅ 2.1s مقاسة (0b825a3) |
| روتين نشر موحد + نسخ احتياطية + تراجع بأمر | ✅ scripts/deploy_161.sh (ب5/ب6) |
| نسخ احتياطية قائمة على 161 | ✅ `/home/dz161/backups/*20260822-120708.*` |

---

## R1 — نشر آخر ثنائية (وضع UDP افتراضياً — صفر تغيير سلوكي)

**الغرض:** نقل كل إصلاحات الأمان/الاستقرار/الأداء للإنتاج دون تغيير البروتوكول.
**التأثير:** إعادة تشغيل وحيدة للحاوية (~5-10s)؛ الأعضاء يعيدون الاتصال تلقائياً
(مُثبت: تعافٍ 2.1s).

```bash
# محلياً: بناء musl من HEAD
cd ~/zgalaxy-work/zgalaxy-rs
TC=$(echo ~/.rustup/toolchains/stable-*/bin)
$TC/cargo build --release --target x86_64-unknown-linux-musl
python3 ~/zgalaxy-work/bin/sshrun.py dz161@192.168.1.161 \
  'echo "147x147x@@##XX" | sudo -S rm -f /tmp/zgrs2'
python3 ~/zgalaxy-work/bin/sshput.py \
  target/x86_64-unknown-linux-musl/release/zgalaxy-rs \
  dz161@192.168.1.161 /tmp/zgrs2

# على 161: النشر الرسمي (نسخ احتياطية ثنائية+volume تلقائياً ثم verify)
python3 ~/zgalaxy-work/bin/sshrun.py dz161@192.168.1.161 \
  'echo "147x147x@@##XX" | sudo -S bash /home/dz161/zgalaxy-rs-scripts/deploy_161.sh deploy /tmp/zgrs2'
```
> إن لم يكن السكربت موجوداً بهذا المسار على الخادم: ارفعه أولاً
> (`sshput.py scripts/deploy_161.sh dz161@... /tmp/deploy_161.sh`) وشغّله من هناك.

**معيار نجاح R1 (سكربت verify يفحصها آلياً):**
- الحاوية healthy؛ سجل الخادم بلا أخطاء؛ `/status` يجيب بعنوان `ef313fb5c9`.
- الشبكات الثلاث محفوظة وقائمة الأعضاء تجيب.
- ztnet (:3000) يستجيب والأعضاء يظهرون كما كانوا.
- md5 الثنائية المنشورة = المصدر.

**التراجع الفوري:** `deploy_161.sh rollback <STAMP>` (يطبع أثناء النشر).

---

## R2 — استيعاب QUIC على حاويات الاختبار (بوابة قبل الإنتاج)

1. شغّل `/tmp/b1_test.sh` بالثنائية النهائية → **B1-PASS**.
2. **المسار غير المختبر بعد (إلزامي هنا):** دورة التخويل من **واجهة ztnet
   الرسومية نفسها** (وليس REST): افتح ztnet → الشبكة التجريبية → فوّض العضو →
   راقب OK+IP على العقدة. (واجهة REST مطابقة لها لكن البوابة تشترط الواجهة.)
3. اختبار ضغط/نقع: ping مستمر + حمل حزم ~512B لمدة ≥30 دقيقة بين zg-a/zg-b
   مع مراقبة فقد/jitter وعدّاد الإسقاطات.
4. اختبار تعافٍ موثق: `docker restart zg-b` → قياس join→ping (≤5s).

**بوابة العبور إلى R3:** كل ما سبق أخضر + قرار مالك صريح.

---

## R3 — تفعيل transportMode=quic على الإنتاج

### ⚠️ نقطة قرار D1 — توافق الأعضاء (اقرأ قبل التنفيذ)
وضع QUIC الحالي **لا يشغّل مسار UDP القديم بالتوازي**. أي عضو لا يزال على عميل
لا يدعم QUIC سيفقد مزامنة التكوين عند التبديل. لذا قبل R3 يجب أحد أمرين:
- **(أ)** ترقية كل أعضاء الشبكة المستهدفة إلى zgalaxy-rs بنفس القدرة (R1-like
  لكل عضو)، أو
- **(ب)** إضافة وضع مزدوج (QUIC+UDP listener معاً) كتحسين لاحق — غير منفذ الآن.

**جرد الوضع الحالي (تحقق سريع قبل القرار):**
```bash
# على 161: من هم الأعضاء؟
docker exec zerotier sh -c 'T=$(cat /var/lib/zerotier-one/authtoken.secret); \
  curl -s -H "X-ZT1-Auth: $T" http://127.0.0.1:9993/controller/network'
# ولكل شبكة: قائمة الأعضاء + lastSeen (عضو بlastSeen حي = عميل QUIC حديث)
```

### خطوات R3 (بعد حسم D1)
```bash
# 1) تفعيل الوضع داخل بيانات الحاوية
echo "147x147x@@##XX" | sudo -S docker exec zerotier sh -c '
  CFG=/var/lib/zerotier-one/local.conf
  jq ".settings.transportMode = \"quic\"" $CFG > /tmp/lc && mv /tmp/lc $CFG
  cat $CFG'

# 2) إعادة تشغيل الحاوية
echo "147x147x@@##XX" | sudo -S docker restart zerotier

# 3) مراقبة دقيقة (أول 5 دقائق)
watch: docker logs -f zerotier            # أخطاء إقلاع QUIC
curl    :3000 ztnet                       # الأعضاء ONLINE؟
docker exec zerotier zerotier-cli listnetworks   # حالة OK لكل شبكة
```

### معايير نجاح R3
- كل عضو مفوّض يصل OK بعنوان من الـpool خلال ≤10s من انضمامه.
- `/peer` يعرض الجذر/المتحكم بـlatency حقيقي.
- ztnet بلا أخطاء والأعضاء ONLINE خضراء.
- حركة ICMP/TCP فعلية بين عضوين عبر TAP↔QUIC (nsenter ping).

### التراجع من R3 (استرجاع فوري للوضع UDP)
```bash
echo "147x147x@@##XX" | sudo -S docker exec zerotier sh -c '
  CFG=/var/lib/zerotier-one/local.conf
  jq "del(.settings.transportMode)" $CFG > /tmp/lc && mv /tmp/lc $CFG'
echo "147x147x@@##XX" | sudo -S docker restart zerotier
# أو تراجع كامل: bash /tmp/deploy_161.sh rollback <stamp>
```

---

## المرحلة 3 (المسار غير الحساسي الموازي) — واجهة Slint

- الهيكل: `~/zgalaxy-work/zgalaxy-rs/zgalaxy-ui/` (main.rs + ui/app.slint).
- المواصفات الكاملة: `~/zt/zgalaxy-rs/info/وثيقة_مواصفات_واجهة_عميل_شبكة_Windows_Rust_Slint.md`.
- أولى المحطات (حسب EXECUTION_PLAN): SystemTray حقيقي، نوافذ
  Networks/Preferences/About/Advanced، Windows-first build، ربط REST فقط
  (**قاعدة: لا منطق شبكة داخل الواجهة**).
- بناء: cargo ضمن workspace zgalaxy-ui؛ Windows عبر CI multi-arch القائم
  (إضافة target x86_64-pc-windows-msvc job) أو cross محلياً.

---

## سجل قرارات التنفيذ (يُملأ أثناء التقدم)

| التاريخ | المرحلة | المنفذ | النتيجة | Stamp/Rollback |
|---|---|---|---|---|
| 2026-08-22 | ب5/ب6 (دورة سابقة) | deploy+verify | PASS | 20260822-120708 |
| 2026-08-23 | R1 — ثنائية `bcde6efefb0f` (HEAD be62a4c، musl static-pie) | deploy+verify | **VERIFY-PASS**: حاوية healthy، `/status` ef313fb5c9 online، 3 شبكات+أعضاء محفوظة، ztnet يستجيب، md5 مطابق | 20260823-151624 |
| 2026-08-23 | R2/1 — b1_test.sh بالثنائية المنشورة | E2E عقدتان | **B1-PASS**: OK+10.244.0.10 تلقائي، ping ‏3/3 ‏0% ‏~0.87ms، توكن عضوية حي في /network، تحدي أ3 في السجل | — |
| 2026-08-23 | R2/2 — تعافٍ موثق (/tmp/b4_recovery.sh) | restart zg-b | **B4-PASS**: هوية محفوظة، join→ping ‏**2.1s** (≤5s)، ping ‏3/3 ‏0% | — |
| 2026-08-23 | R2/3 — نقع ‏31 دقيقة (/tmp/soak_r2.sh) | ping 0.2s + حمل UDP ‏~512B ‏~100pps | **PASS بالبيانات**: ‏8959/8959 رزمة، فقد **0%**، صفر timeout، RTT ‏min/avg/max/mdev = ‏0.26/**0.74**/2.07/**0.10ms**، عدّادات TAP صفر errors/dropped (B: TX ‏114484). ملاحظة: السكربت طبع `SOAK-FAIL` كذباً — خلل منطق حكمه (`timeout` يرجع ‏124)؛ عولج في النسخة الحالية على ‏161 | — |
| 2026-08-23 | R2/4 — بوابة تخويل من واجهة ztnet الرسومية | ☐ | **متبقية** — تتطلب تفاعل بشري/متصفح؛ لا تُنفَّذ آلياً | — |
| 2026-08-23 | D1 — جرد توافق الأعضاء (للقراءة فقط) | /controller/network + member | **محصور**: عضو حي واحد فقط (`5b07896437`، ‏clientVersion 1.3.0، ساعة حية)؛ `4633bff774` و`c1aa29b20e` بـlastSeen=0 (قدماء/غير متصلين) → تبديل R3 الآن يقطع عضوين من ثلاثة. الخياران: ترقية العضوين لzgalaxy-rs أو وضع مزدوج QUIC+UDP (غير منفذ) | — |
| 2026-08-23 | مرحلة 3/P1 — Slint: Tray حقيقي + نوافذ | zgalaxy-ui | **مُنفّذة ومختبرة دخانياً**: AppTray‏ (قائمة ديناميكية كاملة) + AdvancedWindow + PreferencesWindow (إعدادات محفوظة) + AboutWindow. بناء وclippy نظيفان، دخان ‏8s ضد REST وهمي: صفر أخطاء واستقصاء مؤكد | — |
| 2026-08-23 | مرحلة 4 جزئياً — نقطة نهاية إعدادات الشبكة | daemon + zgalaxy-ui | **مغلقة بالاختبار**: POST ‏/network/{id} بجسم اختياري (أعلام allow* جزئياً) + حقول حقيقية في GET + فرض فعلي في مسار المضيف (allowManaged يحكم التطبيق؛ allowDefault يمنع 0.0.0.0/0) + ربط Advanced UI بمانع وميض. ‏43 اختباراً أخضر، clippy نظيف. الثنائية **غير منشورة بعد** — تدخل النشر القادم (R1-like) | — |

| 2026-08-23 | R1 تعزيزي ثانٍ — ثنائية `036153fea4aa` (نقطة الإعدادات) | deploy+verify | **VERIFY-PASS**: healthy، ef313fb5c9 online، 3 شبكات محفوظة، ztnet سليم | 20260823-170948 |
| 2026-08-23 | تحقق حي من نقطة الإعدادات على الإنتاج (/tmp/settings_check.sh داخل الحاوية) | معرّف اختبار مؤقت `00000000deadbeef` | **PASS دورة كاملة**: join+إعدادات → GET محفوظ → تحديث جزئي (allowDefault=false والمتبقي لم يُمسّ) → DELETE تنظيف → `/network` عادت `[]` بصفر أثر | — |

## ملاحظة نشر مهمة
أُغلقت: الثنائية المنشورة حالياً على الإنتاج هي `036153fea4aa` (2026-08-23)
وتتضمن نقطة إعدادات الشبكة؛ أي تعديل لاحق يتطلب بناء musl جديداً ودورة
deploy_161.sh كاملة.
| — | R3 | ☐ | ☐ | ☐ |
