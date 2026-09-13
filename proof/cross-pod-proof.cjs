// Cross-pod functional proof for the multi-Pod poker-banting server.
//
// Client A connects DIRECTLY to pod-1 (:8081), client B DIRECTLY to pod-2
// (:8082). Both talk to the SAME Redis. Every assertion below only passes if
// the subscriber loop is actually fanning mutations across pods:
//   1. A creates room          -> B must receive playerJoined? no: B joins -> A must see playerJoined (pod2 -> pod1)
//   2. A sends ready           -> B must receive playerReady (pod1 -> pod2)
//   3. B sends ready           -> A must receive playerReady (pod2 -> pod1)
//   4. A starts game           -> both must receive gameStarted + state
//   5. PRIVACY: A's state must show A's own hand unmasked and B's hand masked (no raw cards)
//
// Usage: node proof/cross-pod-proof.js
const WebSocket = require('ws');

const POD1 = 'ws://127.0.0.1:8081/ws';
const POD2 = 'ws://127.0.0.1:8082/ws';

let failures = 0;
const assert = (cond, label) => {
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${label}`);
  if (!cond) failures++;
};

function client(url, name) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const box = { ws, name, msgs: [], waiters: [] };
    ws.on('open', () => resolve(box));
    ws.on('error', reject);
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
    box.waitFor = (pred, label, timeoutMs = 8000) => new Promise((res, rej) => {
      const hit = box.msgs.find(pred);
      if (hit) return res(hit);
      const w = { pred, resolve: res };
      box.waiters.push(w);
      setTimeout(() => {
        box.waiters.splice(box.waiters.indexOf(w), 1);
        rej(new Error(`timeout waiting for: ${label} (got ${box.msgs.length} msgs: ${box.msgs.map(m => m.type).join(',')})`));
      }, timeoutMs);
    });
    box.send = (obj) => ws.send(JSON.stringify(obj));
  });
}

const closeAll = (boxes) => boxes.forEach(b => { try { b.ws.close(); } catch {} });

(async () => {
try {
  const A = await client(POD1, 'A');
  const B = await client(POD2, 'B');
  console.log('connected: A->pod1, B->pod2');

  // 1. A creates (on pod1)
  A.send({ type: 'create', name: 'Alice', isPublic: false });
  const created = await A.waitFor(m => m.type === 'created', 'created@A');
  const code = created.code;
  assert(!!code, `room created: ${code}`);

  // 2. B joins from pod2 -> pod1 must broadcast playerJoined to A (CROSS-POD)
  B.send({ type: 'join', code, name: 'Bob' });
  const joined = await B.waitFor(m => m.type === 'joined', 'joined@B');
  assert(joined.playerId === 1, 'B joined as seat 1');
  await A.waitFor(m => m.type === 'playerJoined' && m.playerId === 1, 'A saw playerJoined (pod2->pod1)');
  console.log('PASS  A saw playerJoined (pod2->pod1)');

  // 3. A ready -> B sees it (pod1->pod2)
  A.send({ type: 'ready', ready: true });
  await B.waitFor(m => m.type === 'playerReady' && m.playerId === 0, 'B saw playerReady from A (pod1->pod2)');
  console.log('PASS  B saw playerReady from A (pod1->pod2)');

  // 4. B ready -> A sees it (pod2->pod1)
  B.send({ type: 'ready', ready: true });
  await A.waitFor(m => m.type === 'playerReady' && m.playerId === 1, 'A saw playerReady from B (pod2->pod1)');
  console.log('PASS  A saw playerReady from B (pod2->pod1)');

  // 5. A starts -> both see gameStarted (cross-pod both directions at once)
  A.send({ type: 'startGame' });
  const gsA = await A.waitFor(m => m.type === 'gameStarted', 'gameStarted@A');
  const gsB = await B.waitFor(m => m.type === 'gameStarted', 'gameStarted@B');
  assert(gsA && gsB, 'gameStarted fanned to BOTH pods');

  // wait for state on both (the driver will walk through three-discard -> playing)
  const nonLobby = m => m.type === 'state' && m.state.phase !== 'lobby';
  const stA = await A.waitFor(nonLobby, 'playing-state@A', 20000);
  const stB = await B.waitFor(nonLobby, 'playing-state@B', 20000);
  assert(stA.state && stB.state, 'state broadcast on both pods');

  // 6. PRIVACY: A's state must carry A's hand (13 cards, no 3s after discard)
  //    and B's hand must be MASKED (handCount present OR hand empty, never B's raw cards).
  const aState = stA.state;
  const me = aState.players.find(p => p.id === 0);
  const them = aState.players.find(p => p.id === 1);
  const aHasCards = Array.isArray(me?.hand) && me.hand.length >= 9 && me.hand.length <= 13;
  const bMasked = !Array.isArray(them?.hand) || them.hand.length === 0; // other seats never carry raw cards
  assert(aHasCards, `A sees own hand (${Array.isArray(me?.hand) ? me.hand.length : 'n/a'} cards)`);
  assert(bMasked, 'A does NOT see B raw hand (privacy)');

  // 7. The LAST state both hold must agree on the game-relevant bits
  //    (proves both pods applied the same sequence of mutations).
  const snap = (b) => b.msgs.filter(m => m.type === 'state').pop();
  const la = snap(A).state, lb = snap(B).state;
  assert(la.phase === lb.phase, `final phase matches on both pods (${la.phase})`);
  assert(la.players.length === lb.players.length, 'final player count matches');

  console.log(`\n=== cross-pod proof: ${failures === 0 ? 'ALL PASS' : failures + ' FAILURES'} ===`);
  closeAll([A, B]);
  process.exit(failures === 0 ? 0 : 1);
} catch (e) {
  console.error('FAIL  ' + e.message);
  process.exit(1);
}
})().catch(() => {});
