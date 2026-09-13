// Concurrent-join race proof (scaling gap #3).
//
// Proves the advisory lock `room:lock:{code}` serializes cross-pod mutations:
// N clients join the SAME room at the same instant, split across BOTH pods,
// and the room must end consistent:
//   - exactly (max_players - 1) joiners win (capacity 4, host already seated)
//   - winners get DISTINCT player_ids, no duplicates
//   - losers get the clean "Room is full" error (not "Room not found",
//     not a half-committed seat, no panics)
//
// Usage: NODE_PATH=<client>/node_modules node proof/race-join-proof.cjs
// Env:   POD1=http://127.0.0.1:8081  POD2=http://127.0.0.1:8082  N=10

const WebSocket = require('ws');

const POD1 = process.env.POD1 || 'ws://127.0.0.1:8081/ws';
const POD2 = process.env.POD2 || 'ws://127.0.0.1:8082/ws';
const N = Number(process.env.N || 10); // joiners (host makes 4 total cap)

let failures = 0;
const pass = (msg) => console.log(`PASS  ${msg}`);
const fail = (msg) => { failures++; console.log(`FAIL  ${msg}`); };
const check = (cond, msg) => (cond ? pass(msg) : fail(msg));

function open(target) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(target);
    ws.on('open', () => resolve(ws));
    ws.on('error', reject);
  });
}

// Send a message and resolve with the FIRST matching response.
function send(ws, obj, match, timeoutMs = 15000) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => { ws.close(); reject(new Error(`timeout waiting for ${match}`)); }, timeoutMs);
    const h = (data) => {
      let m;
      try { m = JSON.parse(data.toString()); } catch { return; }
      if (match(m)) {
        clearTimeout(t);
        ws.off('message', h);
        resolve(m);
      }
    };
    ws.on('message', h);
    ws.send(JSON.stringify(obj));
  });
}

async function main() {
  console.log(`[race] host via ${POD1}, ${N} joiners split across ${POD1} / ${POD2}`);

  // 1. Host creates a private room on pod-1 (deterministic: we get the code).
  //    Track the latest `state` broadcast on the host socket — every join
  //    re-broadcasts the room state, so the last one after the join burst is
  //    the authoritative final roster.
  const host = await open(POD1);
  let lastState = null;
  host.on('message', (d) => {
    try {
      const m = JSON.parse(d.toString());
      if (m.type === 'state') lastState = m.state;
    } catch {}
  });
  const created = await send(host, { type: 'create', name: 'Host' }, (m) => m.type === 'created');
  const code = created.code;
  check(!!code && created.playerId === 0, `room created: ${code} (host seat 0)`);

  // 2. Open N sockets first (5 pod-1, 5 pod-2), THEN fire all joins in the
  //    same macrotask burst so the requests contend on the Redis lock.
  const joiners = [];
  for (let i = 0; i < N; i++) {
    const target = i % 2 === 0 ? POD1 : POD2;
    joiners.push({ ws: await open(target), pod: target, i });
  }

  const results = await Promise.allSettled(
    joiners.map(({ ws, i }) =>
      send(ws, { type: 'join', code, name: `J${i}` }, (m) => m.type === 'joined' || m.type === 'error')
    ),
  );

  const joined = [], errors = [], other = [];
  results.forEach((r, i) => {
    if (r.status === 'fulfilled') {
      if (r.value.type === 'joined') joined.push({ i, pid: r.value.playerId, code: r.value.code });
      else if (r.value.type === 'error') errors.push({ i, msg: r.value.message });
      else other.push({ i, t: r.value.type });
    } else {
      other.push({ i, t: `promise-rejected: ${r.reason && r.reason.message}` });
    }
  });

  // 3. Invariants.
  check(joined.length === 3, `exactly 3 of ${N} joiners won (capacity 4 - host) — got ${joined.length}`);
  const pids = joined.map((j) => j.pid).sort((a, b) => a - b);
  check(JSON.stringify(pids) === JSON.stringify([1, 2, 3]), `winners got DISTINCT seats [1,2,3] — got [${pids.join(', ')}]`);
  check(errors.length === N - 3, `${N - 3} losers got clean errors — got ${errors.length}`);
  check(errors.every((e) => /full/i.test(e.msg)), `every loser got "Room is full" (got: ${[...new Set(errors.map((e) => e.msg))].join(' | ')})`);
  check(other.length === 0, `no unexpected results (got: ${other.map((o) => o.t).join(' | ') || 'none'})`);

  // 4. Final state consistency: every join re-broadcasts the room state to all
  //    connected clients, so wait briefly after the burst for the last one to
  //    land on the host socket, then assert the roster. (No `ready` needed —
  //    that would start nothing and its reply isn't a state broadcast.)
  await new Promise((r) => setTimeout(r, 1000));
  if (!lastState) fail('no final state broadcast received on host socket');
  const players = (lastState && lastState.players) || [];
  check(players.length === 4, `final state: 4 players in room — got ${players.length}`);
  const names = players.map((p) => p.name).sort();
  const winnerNames = joined.map((j) => `J${j.i}`).sort();
  check(JSON.stringify(names) === JSON.stringify(['Host', ...winnerNames]), `player roster = host + winners — got [${names.join(', ')}]`);
  const ids = players.map((p) => p.id);
  check(new Set(ids).size === ids.length, `no duplicate player ids — got [${ids.join(', ')}]`);

  [host, ...joiners].forEach(({ ws }) => { try { ws.close(); } catch {} });
  console.log(`\n=== race-join proof: ${failures === 0 ? 'ALL PASS' : failures + ' FAILURE(S)'} ===`);
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => { console.error('FATAL', e); process.exit(2); });
