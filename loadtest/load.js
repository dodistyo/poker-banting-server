// k6 load test for the multi-Pod poker-banting server.
//
// Target is the nginx LB (round-robin across the two backend pods), so each
// VU lands on a random pod while its room state lives in shared Redis. The
// goal is to load the cross-Pod hot path: JSON room reads/writes + Redis
// SET + PUBLISH + PSUBSCRIBE fan-out to the other pod + per-player state
// broadcast.
//
// PROTOCOL (verified against protocol.rs, ws.rs, game/rules.rs, game/engine.rs)
//   * create    = {type:'create', name, isPublic?}     (name, NOT playerName)
//   * ready     = {type:'ready', ready: true}          (start_game requires
//                                                       all humans ready)
//   * startGame = {type:'startGame'}
//   * play      = {type:'play', cards:["rank:suit"]}   STRING ids, e.g.
//                                                       "9:hearts". Card objects
//                                                       make serde reject the
//                                                       whole message -> the
//                                                       server no-ops the move
//                                                       -> the 10s play-limit
//                                                       watchdog fires.
//   * pass      = {type:'pass'}                        always legal.
//   * state.players[]: {id, name, hand:[{rank,suit}], ...} — the seat id
//     field is `id` (NOT playerId). state.currentPlayer is also an `id`.
//   * state.trick = {cards, comboPlayer, comboType, passed, played}
//
// MOVE STRATEGY (mirrors the server's own idle_auto_move in rules.rs):
//   * leading (trick.comboPlayer == null): the lowest single card is always
//     legal.
//   * following a single: only a STRICTLY lower rank is legal; otherwise
//     pass.
//   * table is a bomb: only a higher bomb or pass; VUs just pass.
//   (3s are discarded at game start, so a single 3 can never appear at the
//   table — bombs only matter as "unanswerable", which we handle by passing.)
//
// This makes every VU move legal and prompt, so play_to_nextturn_ms measures
// the real write -> SET + PUBLISH -> PSUBSCRIBE -> broadcast round trip,
// NOT the 10s play-limit watchdog. A p95 near 10s would mean the watchdog.
//
// k6 version: written for the v0.x WS API (ws.connect + socket.on callbacks).
// The v2.x k6/websockets client does not deliver received messages in this
// environment — keep this script on v0.x (tested: k6 v0.57.0).
//
// DO NOT sleep() inside the ws.connect callback: that starves k6's event
// loop and message delivery stops. Poll a shared flag from the iteration
// body with k6's sleep().
//
// RUN:
//   k6 run load.js
//   k6 run -e VUS=20 -e DURATION=5m load.js
//   k6 run -e TARGET=ws://127.0.0.1:8081/ws load.js   # single pod
//   (this machine: /home/dodi/.local/bin/k6-stable = v0.57.0)

import ws from 'k6/ws';
import { sleep } from 'k6';
import { Trend, Counter } from 'k6/metrics';

const VUS = Number(__ENV.VUS ?? 10);
const DURATION = __ENV.DURATION ?? '2m';
const TARGET = __ENV.TARGET ?? 'ws://127.0.0.1:80/ws';
const MAX_SECS = Number(__ENV.MAX_SECS ?? 90); // per-iteration safety cap

// 3D poker-banting ranks, low (3) to high (2). Matches game/card.rs.
const RANK = { '3': 0, '4': 1, '5': 2, '6': 3, '7': 4, '8': 5, '9': 6, '10': 7, J: 8, Q: 9, K: 10, A: 11, '2': 12 };

const tConnect = new Trend('ws_connecting_ms', true);
const tCreate = new Trend('room_create_ms', true);
const tStart = new Trend('game_start_ms', true);
const tPlay = new Trend('play_to_nextturn_ms', true);
const passes = new Counter('turn_passes');
const serverErrors = new Counter('server_errors');
const failed = new Counter('iteration_failed');

export const options = {
  vus: VUS,
  duration: DURATION,
  thresholds: {
    ws_connecting_ms: ['p(95)<250'],
    room_create_ms: ['p(95)<1000'],
    game_start_ms: ['p(95)<1500'],
    // < 3s = a legal move was accepted and the turn advanced promptly.
    // ~10s = the move was dropped and the play-limit watchdog fired.
    play_to_nextturn_ms: ['p(95)<3000'],
    server_errors: ['count<1'],
  },
};

function wire(card) {
  return `${card.rank}:${card.suit}`;
}

// Pick a guaranteed-legal move for my hand vs. the current trick.
// Returns "play" with a single card, or "pass".
function pickMove(hand, trick) {
  if (!hand || hand.length === 0) return { kind: 'pass' };
  const leading = !trick || trick.comboPlayer === null || !Array.isArray(trick.cards) || trick.cards.length === 0;
  if (leading) {
    let best = hand[0];
    for (const c of hand) if (RANK[c.rank] < RANK[best.rank]) best = c;
    return { kind: 'play', card: best };
  }
  const table = trick.cards[0];
  if (trick.comboType !== null && trick.comboType !== undefined && trick.comboType !== 'single') {
    // Following a pair/triple/... or a bomb: matching combos are rare for a
    // 13-card hand and bombs can't lead, so just pass — always legal.
    return { kind: 'pass' };
  }
  // Following a single: only a strictly lower rank is legal.
  const tableRank = RANK[table.rank];
  let best = null;
  for (const c of hand) {
    if (RANK[c.rank] < tableRank && (best === null || RANK[c.rank] < RANK[best.rank])) best = c;
  }
  return best ? { kind: 'play', card: best } : { kind: 'pass' };
}

export default function () {
  const t0 = Date.now();
  const st = {
    myPid: null,
    started: false,
    actionSentAt: null, // when my legal play/pass went out
    done: false,
  };
  let socket = null;

  ws.connect(TARGET, null, (s) => {
    socket = s;
    s.on('open', () => {
      tConnect.add(Date.now() - t0);
      s.send(JSON.stringify({ type: 'create', name: `k6-${__VU}-${Date.now()}` }));
    });
    s.on('message', (raw) => {
      let m;
      try {
        m = JSON.parse(raw);
      } catch {
        return;
      }
      switch (m.type) {
        case 'created':
          st.myPid = m.playerId;
          tCreate.add(Date.now() - t0);
          s.send(JSON.stringify({ type: 'ready', ready: true }));
          s.send(JSON.stringify({ type: 'startGame' }));
          break;
        case 'gameStarted':
          st.started = true;
          tStart.add(Date.now() - t0);
          break;
        case 'state': {
          if (!st.started || !m.state || !m.state.players || st.done) break;
          const me = m.state.players.find((p) => p.id === st.myPid);
          if (!me) break;
          if (m.state.phase === 'playing' && m.state.currentPlayer === st.myPid) {
            const mv = pickMove(me.hand, m.state.trick);
            if (mv.kind === 'play') {
              s.send(JSON.stringify({ type: 'play', cards: [wire(mv.card)] }));
            } else {
              s.send(JSON.stringify({ type: 'pass' }));
              passes.add(1);
            }
            st.actionSentAt = Date.now();
          } else if (m.state.phase === 'playing' && m.state.currentPlayer !== st.myPid && st.actionSentAt !== null) {
            // Turn moved past me: my legal move was accepted and the driver
            // advanced the round. That is the real cross-pod hot path.
            st.done = true;
            tPlay.add(Date.now() - st.actionSentAt);
          }
          break;
        }
        case 'error':
          serverErrors.add(1);
          break;
      }
    });
  });

  // Poll with k6's sleep so the event loop keeps delivering WS messages.
  let waited = 0;
  while (waited < MAX_SECS && !st.done) {
    sleep(0.5);
    waited += 0.5;
  }

  if (!st.done) failed.add(1);
  if (socket) socket.close();
}
