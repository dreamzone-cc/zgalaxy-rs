# تشغيل ztnet فوق zgalaxy-rs — دليل التكامل والاستبدال الكامل

> **الفكرة الجوهرية:** ‏zgalaxy-rs يُثبَّت **بدل** zerotier-one بالكامل في منظومة
> ztnet — ليس بجانبه ولا كطبقة فوقه. الحاوية التي يراها ztnet باسم `zerotier`
> تُشغّل فعلياً محرك Rust الآمن ذا الذاكرة، وتنشأ هكذا من اللحظة الأولى
> (التثبيت الجديد) أو تُرحَّل إليها من منظومة قائمة (الترحيل في المكان).
>
> آخر تحديث: 2026-08-23 · الحزمة: `deploy/ztnet/` · مُجرَّب على الإنتاج (dz161).

---

## 1. المعمارية

```text
┌────────────────────────── docker network: app-network (172.31.255.0/29) ──┐
│                                                                            │
│  ┌─────────────────┐        ┌──────────────────────────────────────┐       │
│  │   postgres       │        │  ztnet (الصورة الرسمية sinamics/ztnet) │       │
│  │  postgres:15.2   │◄───────│  - يتصل بالمتجر عبر POSTGRES_*        │       │
│  └─────────────────┘        │  - ZT_ADDR=http://zerotier:9993       │       │
│                              │  - يقرأ authtoken.secret من المجلد    │       │
│                              │    المشترك (وضع المتحكم المحلي الموثق) │       │
│                              └──────────────┬───────────────────────┘       │
│                                             │ HTTP + X-ZT1-Auth            │
│                                             ▼                              │
│  ┌─────────────────────────────────────────────────────────────┐           │
│  │  zerotier  (الاسم تعاقدي فقط — المحرك zgalaxy-rs!)           │           │
│  │  image: zgalaxy-zerotier:rs                                   │           │
│  │  - /usr/sbin/zerotier-one  = zgalaxy-rs (musl static)         │           │
│  │  - zerotier-cli / zerotier-idtool = روابط رمزية (argv0)       │           │
│  │  - REST محلي 9993: /status /network /controller/* /peer      │           │
│  │  - volume: zerotier → /var/lib/zerotier-one (identity+token)  │           │
│  │  - TAP حقيقي + QUIC + متحكم مدمج متوافق 100% مع ztnet          │           │
│  └──────────────────────────────────────────────────────────────┘           │
└────────────────────────────────────────────────────────────────────────────┘
```

**لماذا يبقى اسم الخدمة `zerotier`؟** لسببين: (1) ‏ztnet يصل للمتحكم عبر اسم
المضيف `http://zerotier:9993`، و(2) كل أدوات التشغيل القائمة
(‏`scripts/deploy_161.sh`، اختبارات ب1، دليل ROLLOUT) تتوقع الحاوية بهذا
الاسم — فلا شيء ينكسر عند التبديل.

## 2. عقد التوافق الأربعة (ما يجعل الاستبدال شفافاً)

| العقد | التفاصيل |
|---|---|
| **اسم الثنائية** | ‏zgalaxy-rs يُنسخ كـ`/usr/sbin/zerotier-one` وينتظر أوامره عبر argv0: ‏`zerotier-one` ‏(daemon)، ‏`zerotier-cli` ‏(واجهة)، ‏`zerotier-idtool` ‏(أدوات الهوية) |
| **REST API** | نفس دلالات ZeroTier المحلية: `X-ZT1-Auth` + `/status` و`/network` ‏(join/leave/إعدادات) و`/controller/network/*` ‏(المتحكم — نفس واجهة ztnet) و`/peer` |
| **مخطط البيانات** | ‏`/var/lib/zerotier-one` بنفس الملفات: `identity.secret/public`، `authtoken.secret`، `local.conf`، `networks.d/`، عالم planet |
| **الصحة** | ‏healthcheck عبر `zerotier-cli status` (يستعلم `/status` داخلياً) — نفس سلوك صورة zyclonite الأصلية |

## 3. تفعيل إدارة ztnet — local.conf (احذر الفخ)

يجب أن يسمح `allowManagementFrom` للشبكة الفرعية لdocker (‏`172.31.255.0/29`
افتراضياً). **الصيغة الكانونية لZeroTier تضع المفتاح داخل `settings`:**

```json
{
  "settings": {
    "portMappingEnabled": true,
    "softwareUpdate": "disable",
    "allowManagementFrom": ["172.31.255.0/29"]
  }
}
```

> ⚠️ **فخ مؤرخ (اكتشف وأُصلح 2026-08-23):** ‏zgalaxy-rs كان يستخلص
> `primaryPort` من `settings` لكنه يُغفل `allowManagementFrom` — فتظل القائمة
> الفعلية loopback فقط، ويُرفض كل نداء من ztnet برسالة
> `refused by allowManagementFrom`، وأول عارض ظاهر هو فشل
> "Create a network" في الواجهة مع `error fetching http://zerotier:9993/status`
> في سجل ztnet. النسخة `1a22b4a0` وما بعدها تدعم الصيغتين (علوية وداخل
> settings) وثبّتها ثلاثة اختبارات وحدة في `src/config.rs`.

سكربت `setup.sh` يكتب هذا الملف تلقائياً ويُعيد التحميل — لا تكتبه يدوياً إلا
للطلب الخاص (شبكة إدارة مختلفة: مرّر `ZTNET_MANAGE_CIDR` وعدّل subnet في
compose معاً).

## 4. التثبيت الجديد (fresh) — منظومة كاملة بلا zerotier إطلاقاً

### آلياً (الموصى به)
```bash
# 1) على جهاز البناء: ثنائية musl
cargo build --release --target x86_64-unknown-linux-musl

# 2) انقل الثنائية والمستودع (أو deploy/ztnet وحدها) إلى الخادم ثم:
cd zgalaxy-rs/deploy/ztnet
SUDO_PASS='...' ./setup.sh fresh \
  --bin /path/to/zgalaxy-rs \
  --url http://<SERVER-IP>:3000

# أو على خادم فيه المستودع وcargo (يبني بنفسه):
SUDO_PASS='...' ./setup.sh fresh --repo ~/zgalaxy-work/zgalaxy-rs \
  --url http://<SERVER-IP>:3000
```
السكربت: يبني صورة `zgalaxy-zerotier:rs` ← ينشئ `.env` بـ`NEXTAUTH_SECRET`
مولّد ← يشغّل postgres + المحرك + ztnet ← ينتظر الصحة ← يكتب `local.conf` ←
ينفّذ التحقق الكامل (بما فيه مسار ztnet→المتحكم).

آخر خطوة يدوية واحدة: افتح `http://<SERVER-IP>:3000` وأنشئ حساب المسؤول —
هذه دورة ztnet نفسها ولا يمكن (ولا يجب) أتمتتها من الخارج. المتحكم المحلي
يظهر تلقائياً بفضل `ZT_ADDR`.

### يدوياً (لفهم كل قطعة)
```bash
cargo build --release --target x86_64-unknown-linux-musl
docker build -f deploy/ztnet/Dockerfile -t zgalaxy-zerotier:rs .
cd deploy/ztnet
export NEXTAUTH_URL=http://<SERVER-IP>:3000
export NEXTAUTH_SECRET="$(openssl rand -base64 32)"
docker compose -p ztnet up -d
# ثم اكتب local.conf كما في §3 وأعد تشغيل الحاوية، وتحقق بـ setup.sh verify
```

## 5. الترحيل في المكان (migrate) — من zerotier قائم إلى zgalaxy-rs

لخادم يشغّل منظومة ztnet أصلية (صورة `zyclonite/zerotier`):

```bash
cd zgalaxy-rs/deploy/ztnet
SUDO_PASS='...' ./setup.sh migrate --dir ~/ztnet --bin /path/to/zgalaxy-rs
```

ما يفعله السكربت (بالترتيب):
1. يسجّل عنوان الهوية الحالي (يجب أن يبقى ثابتاً عبر التبديل)
2. **نسخة احتياطية كاملة**: ملف compose + أرشيف tar لمجلد المحرك بأكمله
   (الهوية، الشبكات، الأعضاء، التوكن) — قبل لمس أي شيء
3. يستبدل سطر صورة الخدمة `zyclonite/zerotier:*` بـ`zgalaxy-zerotier:rs`
   في compose (الاسم `zerotier` والمجلد والشبكة كما هما)
4. يعيد إنشاء الحاوية — **نفس volume = نفس الهوية والشبكات والأعضاء**
5. يكتب `local.conf` بـ`allowManagementFrom` ويعيد التحميل
6. يتحقق أن العنوان لم يتغير + التحقق الكامل

**عند أي فشل:** ‏`SETUP-FAIL` يظهر مع خطوة التعثر؛ استرجاع الحالة القديمة =
إرجاع `.bak` في compose واستعادة أرشيف المجلد:
```bash
docker stop zerotier && docker volume rm ztnet_zerotier
docker volume create ztnet_zerotier
docker run --rm -v ztnet_zerotier:/data -v ~/backups:/bk alpine \
  sh -c "cd /data && tar xzf /bk/zerotier-volume.<STAMP>.tgz"
cd ~/ztnet && docker compose up -d zerotier
```

## 6. تحديث ثنائية المحرك (دورة نشر كاملة)

```bash
SUDO_PASS='...' ./setup.sh update-binary --bin /tmp/zgrs5   # نسخ احتياطي → تبديل → تحقق
./setup.sh rollback 20260823-181253                          # تراجع بأمر واحد
```
(نفس منطق `scripts/deploy_161.sh` المجرّب — يبقى صالحاً لأن اسم الحاوية لم يتغير.)

## 7. التحقق — ماذا يعني VERIFY-PASS

| الفحص | يثبت |
|---|---|
| الحاويات الثلاث تعمل + المحرك healthy | الإقلاع والـhealthcheck التوافقي |
| `/status` يجيب بالعنوان والنسخة | REST المحلي والتوكن |
| `/controller/network` يسرد الشبكات | المتحكم المدمج حي |
| ztnet-http: 200/30x | واجهة الويب |
| **مسار ztnet→المتحكم HTTP 200** | allowManagementFrom مقروء صحيحاً (فخ §3) |
| صفر `refused` في السجل | لا رفضات صامتة |

## 8. استكشاف الأخطاء

| العرَض | السبب الغالب | الحل |
|---|---|---|
| "Could not create network" في ztnet | `allowManagementFrom` لا يُقرأ من `settings` (نسخة < `1a22b4a0`) | حدّث الثنائية؛ أو أضف الحقل علوياً مؤقتاً؛ تحقق §3 |
| `refused by allowManagementFrom` في سجل المحرك | subnet الشبكة ليست في القائمة | طابق `ZTNET_MANAGE_CIDR` مع subnet في compose |
| المحرك لا يقلع / غير healthy | الثنائية ليست musl (‏glibc المضيف أحدث من الصورة) | `cargo build --release --target x86_64-unknown-linux-musl` |
| تغيّر عنوان العقدة بعد الترحيل | volume لم يُحفَظ (اسم مختلف) | استرجع أرشيف المجلد (§5) |
| ztnet لا يرى المتحكم المحلي | `ZT_ADDR` غائب أو المجلد غير مشترك | تأكد من متغيري البيئة والمجلد في compose |
| الحاوية تحتاج TAP | `/dev/net/tun` غير ممرّر | devices + cap_add موجودة في compose المرفق |

## 9. قواعد ثابتة (غير قابلة للتفاوض)

- **لا تعديل على ztnet إطلاقاً** — التكامل كله عبر واجهاته الموثقة
  (‏`ZT_ADDR`، `POSTGRES_*`، مجلد authtoken، REST المتحكم).
- QUIC هو النقل المستقبلي؛ لا تفعّل `transportMode=quic` إنتاجياً إلا عبر
  [ROLLOUT-QUIC-PRODUCTION.md](ROLLOUT-QUIC-PRODUCTION.md) وبعد حسم D1.
- كل نشر/تحديث **بنسخة احتياطية وتراجع** — لا استثناءات.
- `zerotier` اسم تعاقدي للحاوية — لا تُعِد تسميته أو سينكسر ztnet والأدوات.

## 10. ملفات الحزمة

| الملف | الدور |
|---|---|
| `deploy/ztnet/Dockerfile` | صورة الاستبدال الكامل (musl كـzerotier-one + الروابط + healthcheck) |
| `deploy/ztnet/docker-compose.yml` | المنظومة الكاملة للتثبيت الجديد (postgres + zgalaxy + ztnet الرسمي) |
| `deploy/ztnet/setup.sh` | الأتمتة: `fresh` / `migrate` / `update-binary` / `verify` / `rollback` |
| `scripts/deploy_161.sh` | نشر/تحقق/تراجع على dz161 تحديداً (ما زال صالحاً بنفس أسماء الحاويات) |
