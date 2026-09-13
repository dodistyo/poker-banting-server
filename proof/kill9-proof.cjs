// KILL-9 PROOF: per-process boot_id liveness (zombie-seat reaper).
//
// Proves the CRITICAL scenario: a pod that dies HARD (SIGKILL — leave_room
// can NEVER run) and is RESPAWNED WITH THE SAME POD_NAME (Cloud Run replica
// restart) does NOT inherit its dead predecessor's seats. Liveness is keyed
// by a per-BOOT id, so the old boot's `pod:alive:{boot}` key expires after
// the TTL, the reaper bot-ifies the orphan seats, and the owner re-joins —
// no "seat in use" deadlock, no waiting for the orphan-room reaper.
//
// Setup: sim stack (pod-1 :8081, pod-2 :8082, shared Redis, TTL 15s).
// A creates on pod-1, B joins on pod-2, game starts. Then:
//   1. `docker exec ... kill -9 1`  -> PID 1 dies, container EXITS
//      (sim restart policy is "no", so we do step 2 manually — this is the
//      Cloud Run respawn: fresh process, SAME POD_NAME, same image).
//   2. `docker start <container>`   -> fresh process, NEW boot_id.
//   3. A reconnects to :8081 and must rejoin seat 0 within one TTL window
//      (the OLD boot key expires; the reaper frees the seat; rejoin works).
//
// A synthetic "set connected=false" would prove nothing: the whole point is
// that leave_room never ran, so the seat stays `connected: true` in Redis
// until the OLD process's liveness key dies and the reaper takes it over.
//
// Usage (with the 2-pod sim stack running):
//   NODE_PATH=<client>/node_modules node proof/kill9-proof.cjs
const WebSocket = require('ws');

const POD1 = 'ws://127.0.0.1:8081/ws';
const POD2 = 'ws://127.0.0.1:8082/ws';
const POD1_CTR = process.env.POD1_CTR || 'poker-banting-server-backend-1-1';
const TTL_MS = (parseInt(process.env.POD_ALIVE_TTL_SECS || '15', 10)) * 1000;

const { execSync } = require('child_process');
const sh = (cmd) => execSync(cmd, { stdio: 'inherit' });
const shq = (cmd) => execSync(cmd, { encoding: 'utf8' }).trim();

let failures = 0;
const assert = (cond, label) => {
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${label}`);
  if (!cond) failures++;
};

function client(url, name, timeoutMs = 8000) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const box = { ws, name, msgs: [], waiters: [] };
    const t = setTimeout(() => reject(new Error(`connect timeout to ${url}`)), timeoutMs);
    ws.on('open', () => { clearTimeout(t); resolve(box); });
    ws.on('error', (e) => { clearTimeout(t); reject(e); });
    ws.on('message', (buf) => {
      let m;
      try { m = JSON.parse(buf.toString()); } catch { return; }
      box.msgs.push(m);
      for (const w of [...box.waiters]) {
        if (w.pred(m)) {
          box.waiters.splice(box.waiters.indexOf(w), 1);
          w.resolve(m);
        }
      }
    });
    box.waitFor = (pred, label, ms = 8000) => new Promise((res, rej) => {
      const hit = box.msgs.find(pred);
      if (hit) return res(hit);
      const w = { pred, resolve: res };
      box.waiters.push(w);
      setTimeout(() => {
        box.waiters.splice(box.waiters.indexOf(w), 1);
        rej(new Error(`timeout waiting for: ${label} (got ${box.msgs.length} msgs: ${box.msgs.map(m => m.type).join(',')})`));
      }, ms);
    });
    box.send = (obj) => ws.send(JSON.stringify(obj));
    box.close = () => { try { box.ws.close(); } catch {} };
  });
}

const sleep = (ms) => new Promise(r => setTimeout(r, ms));

async function waitContainerExit(name, ms = 15000) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const st = shq(`docker inspect -f '{{.State.Status}}' ${name}`);
    if (st === 'exited') return;
    await sleep(250);
  }
  throw new Error(`container ${name} did not exit in ${ms}ms`);
}

(async () => {
  let A, B, A2;
  try {
    A = await client(POD1, 'A');
    B = await client(POD2, 'B');

    // A creates (pod-1), B joins (pod-2) — seats owned by DIFFERENT boots.
    A.send({ type: 'create', name: 'Alice', isPublic: true });
    const created = await A.waitFor(m => m.type === 'created', 'A created');
    const code = created.code;
    const tokenA = created.token;

    B.send({ type: 'join', code, name: 'Bob' });
    const joined = await B.waitFor(m => m.type === 'joined', 'B joined');

    A.send({ type: 'ready', ready: true });
    B.send({ type: 'ready', ready: true });
    await A.waitFor(m => m.type === 'playerReady' && m.playerId === 1, 'B ready (cross-pod)');
    A.send({ type: 'startGame' });
    const started = await A.waitFor(m => m.type === 'state' && m.state.phase === 'playing', 'game started');
    assert(started.state.players.find(p => p.id === 0).connected, 'A connected pre-kill');
    assert(started.state.players.find(p => p.id === 1).connected, 'B connected pre-kill');

    // --- THE KILL: SIGKILL PID 1 inside pod-1. leave_room can never run. ---
    const t0 = Date.now();
    console.log(`--- SIGKILL ${POD1_CTR} (docker kill -s KILL, == kill -9 1) ---`);
    sh(`docker kill -s KILL ${POD1_CTR}`);
    await waitContainerExit(POD1_CTR);
    await new Promise(r => {
      const t = setTimeout(r, 5000);
      A.ws.on('close', () => { clearTimeout(t); r(); });
    });
    console.log(`  (container exited; A's socket dead after ${Date.now() - t0}ms)`);

    // --- RESPAWN: same container name/POD_NAME, fresh process = new boot_id.
    // (Cloud Run does this automatically; sim restart policy is "no".)
    console.log(`--- docker start ${POD1_CTR} (fresh process, same POD_NAME) ---`);
    sh(`docker start ${POD1_CTR}`);

    // --- RECONNECT to :8081 (fresh process). Rejoin must succeed only AFTER
    // the OLD boot's liveness key expires (<= TTL) and the reaper frees the
    // seat. Timeout: TTL + generous margin. ---
    const reconnectDeadline = t0 + TTL_MS + 12000;
    let connected = null;
    for (;;) {
      try { connected = await client(POD1, 'A', 1500); break; }
      catch { if (Date.now() > reconnectDeadline) throw new Error('pod-1 did not come back'); await sleep(500); }
    }
    const A2b = connected; A2 = A2b;
    // Rejoin must be RETRIED until the reaper runs: until the OLD boot's
    // liveness key expires (<= TTL) the seat is still `connected:true`, so
    // the first attempts get "Seat already in use". This mirrors the real
    // client (network.js rejoin attempts loop).
    A2b.send({ type: 'rejoin', code, name: 'Alice', token: tokenA });
    const rejoined = await new Promise((res, rej) => {
      const deadline = Date.now() + TTL_MS + 12000;
      const iv = setInterval(() => {
        if (Date.now() > deadline) { clearInterval(iv); rej(new Error('rejoin never accepted within TTL+12s')); return; }
        try { A2b.send({ type: 'rejoin', code, name: 'Alice', token: tokenA }); } catch {}
      }, 1500);
      A2b.waitFor(m => m.type === 'rejoined', 'A rejoined after kill-9 + respawn', TTL_MS + 13000)
        .then(m => { clearInterval(iv); res(m); })
        .catch(e => { clearInterval(iv); rej(e); });
    });
    const dt = Date.now() - t0;
    assert(rejoined.playerId === 0, `A got back seat 0 (got ${rejoined.playerId})`);
    assert(rejoined.state.phase === 'playing', 'game still running (not orphan-reaped)');
    assert(dt < TTL_MS + 12000, `rejoin in ${dt}ms (TTL window + margin; orphan reaper would take much longer)`);
    console.log(`  (A back in seat 0 after ${dt}ms)`);

    // B must have seen A rejoin (cross-pod fan-out) and still be playing.
    await B.waitFor(m => m.type === 'playerJoined' && m.playerId === 0, 'B saw A rejoin', 10000);
    assert(true, 'B saw A rejoin (fan-out intact)');

    A.close(); A2b.close(); B.close();
  } catch (e) {
    console.error('FAIL  proof error:', e.message);
    failures++;
    if (A) A.close();
    if (A2) A2.close();
    if (B) B.close();
  }
  console.log(failures === 0 ? '\nKILL-9 PROOF: ALL PASS' : `\nKILL-9 PROOF: ${failures} FAILURE(S)`);
  process.exit(failures === 0 ? 0 : 1);
})();
