# Capacity Baseline & Test Targets

_Dokumen permanen: angka capacity yang ter-verify + cara nge-run test ke
target mana pun (local sim, LB, atau production). Sumber angka:
2026-09-13, stack sim `docker-compose.sim.yaml` (1 vCPU / 512 MiB /
nofile 65535 per pod = persis instance Cloud Run production)._

## Capacity per container (1 vCPU / 512 MiB, nofile 65535)

| Skenario | Kapasitas | Bukti |
|---|---|---|
| Pemain aktif — operasional (RAM ~80%) | **~2.500** | k6 2500 VU 1m20s: 100% pass, 0 server error, p95 play 26 ms, RAM 398–433 MiB (78–84%), CPU rata-rata ~20% (spike 39%) |
| Pemain aktif — aman (RAM ~60%) | ~1.300 | ekstrapolasi linear dari data di bawah |
| Koneksi idle (plafon RAM) | ~7.400 | `proof/hold-proof.cjs` N=8000: OOMKilled di 7.435 koneksi |

Model RAM (terverifikasi linear 1500→2500 VU): **~0.14 MiB per koneksi
aktif** + ~34 MiB base. Bottleneck = RAM, bukan CPU (CPU masih ~20%
rata-rata di 2500 VU). Semua run selesai dengan pod kembali idle 0% CPU
(tidak ada wedge — lihat riwayat bug `skip_finished` unbounded, fix di
commit `d40ed49`; regression test anti-hang ada di suite).

### Catatan Cloud Run

Di Cloud Run, 1 WS = 1 in-flight request, jadi ceiling per instance =
`--concurrency` (default **1000**), BUKAN angka RAM di atas. Artinya:

- 1 instance Cloud Run praktis = **~1.000 pemain aktif** (concurrency cap).
- Lebih dari itu → naikkan `--max` (butuh Opsi B: Redis multi-pod) atau
  naikkan limit concurrency per instance.
- 2 pod ≈ x2: **~2.000–5.000 aktif** tergantung `--concurrency` per pod;
  koneksi idle via LB dibatasi `worker_connections` nginx (default 4096).

## Cara nge-test (switchable target)

Semua test di-switch lewat 1 env var, tanpa ubah kode:

### Loadtest (k6) — `loadtest/load.js`

```sh
# 1 pod local (port 8081)
k6 run -e VUS=1500 -e DURATION=1m20s -e TARGET=ws://127.0.0.1:8081/ws loadtest/load.js

# 2 pod via nginx LB local (:8085)
k6 run -e VUS=3000 -e DURATION=1m20s -e TARGET=ws://127.0.0.1:8085/ws loadtest/load.js

# Production (wss; k6 native support TLS)
k6 run -e VUS=1000 -e DURATION=1m20s -e TARGET=wss://api.poker-banting.dodistyo.com loadtest/load.js
```

### Hold test (koneksi idle) — `proof/hold-proof.cjs`

```sh
TARGET=ws://127.0.0.1:8081/ws node proof/hold-proof.cjs    # N/DURATION via env
```

### E2E (playwright) — repo `poker-banting-client`

Browser → `localhost:3000` (dev-server) → `PROXY_TARGET`. Dev-server
TLS-aware: bare `host:port` = http/ws (local), full `https://` URL =
otomatis https + wss (prod).

```sh
# Local 2-pod sim
PROXY_TARGET=127.0.0.1:8085 node dev-server.js

# Local dev single pod
PROXY_TARGET=localhost:8080 node dev-server.js

# Production
PROXY_TARGET=https://api.poker-banting.dodistyo.com node dev-server.js

# Lalu:
E2E_EXTERNAL_BACKEND=1 E2E_HEADLESS=1 npx playwright test
```

Hasil baseline 2026-09-13: e2e 62/62 pass (via LB 2-pod local).

## Verification: Production (Cloud Run, 2026-09-13)

Target: `https://poker-banting-153176493081.asia-southeast1.run.app`
(1 vCPU / 512 MiB, `--concurrency 1000`, `--max 3`, Redis/Upstash SGP).

| Test | Hasil |
|---|---|
| e2e (playwright, via dev-server TLS proxy) | **62/62 pass** (6 menit, headless) |
| k6 burst 2000 CCU, 45 detik | **0 server error**, p95 `play_to_nextturn_ms` **37 ms**, p95 `ws_connecting` 10.8 s |

Catatan 2000 CCU:
- 2000 > `--max 3` × 1000 = 3000? No — 2000 < 3000, jadi **bukan** uji ceiling,
  tapi bukti **scale-out multi-instance** jalan (2 instance = 2000, di atas 1
  instance = 1000 cap). Kalau hanya 1 instance, 2000 VU pasti ada yang
  `503/timeout` di koneksi. Hasil 0 error ⇒ traffic ter-sebar ≥2 instance.
- `ws_connecting` p95 10.8 s = cold start + TLS handshake Jakarta→SGP (RTT
  ~80 ms) + thundering herd 2000 koneksi sekalian. **Bukan** server
  bottleneck: `play_to_nextturn` (post-connect, in-game) tetap 37 ms.
- Upstash free tier: burst 45 detik bakar ~87K command (30K→117K→270K
  harian, termasuk test lain). 17.5 cmd/s/player × 2000 = ~35K cmd/s, jadi
  free tier (500K/bln) = ~14 detik 2000 CCU sustained. Untuk sustained
  2000+ pakai PAYG (budget cap $5).

## Pitfall yang bikin angka keliatan salah (sudah di-fix semua)

1. **`skip_finished` unbounded** (`while players[cp].finished`) — bomb
   endgame nge-mark 4/4 finished → tokio worker spin 100% CPU → semua
   handshake timeout. Fix: bounded `for _ in 0..4` (commit `d40ed49`).
   Kalau pernah lihat "pod hang di 100% CPU setelah banyak game
   selesai", ini dulu — cek regression test-nya hijau.
2. **`nofile` default 1024** — ceiling 1.000-an koneksi. Compose sim
   udah set `ulimits: nofile: 65535`; di Cloud Run pastikan
   `--no-file` / container limit setara.
3. **`docker compose restart` reusing cgroup** bikin CPU reading 100%
   palsu. Pakai `down` + `up` buat baseline bersih.
4. **Cloud Run concurrency ≠ capacity hardware** — lihat catatan di atas.
