// HOLD PROOF: N WebSocket connections held open through the nginx LB for a
// fixed window, then count how many are still alive. This is the "CCU"
// capacity test — the point is that a pod can HOLD this many idle WS under
// its Cloud Run resource limits (1 vCPU / 512 MiB per pod, mirrored in
// docker-compose.sim.yaml).
//
// RUN (needs the sim stack up: docker compose -f docker-compose.sim.yaml up -d):
//   NODE_PATH=<client>/node_modules node proof/hold-proof.cjs
//   N=1000 DURATION=30 node proof/hold-proof.cjs   # overrides
//
// Exit 0 = all (or nearly all) alive. The 1-2% handshake-burst failures under
// 1 vCPU are a known artifact (documented in TECH-DEBT-SCALING.md), not a
// hold failure — a HOLD failure is a socket dying mid-window.

const { execSync } = require('node:child_process');
const WebSocket = require('ws');

const N = Number(process.env.N ?? 1000);
const DURATION = Number(process.env.DURATION ?? 30); // seconds
const LB = process.env.LB ?? 'ws://127.0.0.1:8085/ws';

function dockerStats() {
  try {
    const out = execSync(
      `docker stats --no-stream --format '{{.Name}} {{.CPUPerc}} {{.MemUsage}}'`,
      { encoding: 'utf8' },
    );
    return out
      .split('\n')
      .filter((l) => l.includes('backend'))
      .map((l) => l.trim());
  } catch {
    return ['(docker stats unavailable)'];
  }
}

const sockets = [];
const dead = [];
const connectFail = [];
let connected = 0;
const t0 = Date.now();

function connect(i) {
  return new Promise((resolve) => {
    let done = false;
    const ws = new WebSocket(LB, { handshakeTimeout: 10000 });
    ws.on('open', () => {
      connected++;
      resolve();
    });
    ws.on('error', (e) => {
      if (!done) {
        done = true;
        connectFail.push({ i, err: e.message });
        try { ws.close(); } catch {}
        resolve();
      }
    });
    ws.on('close', (code, reason) => {
      const elapsed = (Date.now() - t0) / 1000;
      if (elapsed < DURATION - 1) {
        dead.push({ i, code, reason: String(reason), at: elapsed.toFixed(1) + 's' });
      }
    });
    sockets.push(ws);
  });
}

async function main() {
  // Connect in batches (near-burst worst case for the 1 vCPU handshake path),
  // spaced 50 ms so the LB isn't overwhelmed by simultaneous SYNs.
  const batch = 50;
  for (let i = 0; i < N; i += batch) {
    const slice = Array.from({ length: Math.min(batch, N - i) }, (_, k) => i + k);
    await Promise.all(slice.map(connect));
    if (i + batch < N) await new Promise((r) => setTimeout(r, 50));
  }

  const connectSecs = ((Date.now() - t0) / 1000).toFixed(1);
  console.log(`connected ${connected}/${N} in ${connectSecs}s (failed: ${connectFail.length})`);
  if (connectFail.length) console.log('  sample fails:', connectFail.slice(0, 5));

  console.log(`--- holding ${connected} sockets for ${DURATION}s ---`);
  setTimeout(() => {
    console.log('mid-hold stats:');
    for (const l of dockerStats()) console.log('  ' + l);
  }, DURATION * 5000);

  await new Promise((r) => setTimeout(r, DURATION * 1000));

  let alive = 0;
  for (const ws of sockets) if (ws.readyState === WebSocket.OPEN) alive++;
  console.log('final stats:');
  for (const l of dockerStats()) console.log('  ' + l);
  console.log(`ALIVE ${alive}/${connected} after ${DURATION}s hold`);
  if (dead.length) {
    console.log('MID-WINDOW DEATHS:', dead.length, '— sample:', dead.slice(0, 10));
    for (const ws of sockets) { try { ws.close(); } catch {} }
    process.exit(2);
  }
  const pct = ((alive / connected) * 100).toFixed(1);
  console.log(alive === connected ? 'HOLD PROOF: ALL PASS (100%)' : `HOLD PROOF: ${pct}% alive (some never fully settled)`);
  for (const ws of sockets) { try { ws.close(); } catch {} }
  process.exit(alive === connected ? 0 : 1);
}

main().catch((e) => {
  console.error('hold proof crashed:', e);
  process.exit(3);
});
