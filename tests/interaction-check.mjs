// Run against a desktop displaying the motion-window fixture. Measures actual changes to the scrolling page and
// dragged window sampled on browser refreshes, and click-to-colour response on the same browser clock.
import { readFileSync } from 'node:fs';
import { sleep, withPage } from './browser.mjs';

const [address, seconds = '8'] = process.argv.slice(2);
const duration = Number(seconds), idle = Number(process.env.IDLE_SECONDS ?? 0);
if (!address || !Number.isFinite(duration) || duration <= 0 || !Number.isFinite(idle) || idle < 0)
  throw Error('Expected an HTTPS address and a positive gesture duration');
const password = process.env.PASSWORD_FILE ? readFileSync(process.env.PASSWORD_FILE, 'utf8').trimEnd() : '';
const result = await withPage(address, async ({ evaluate }) => {
  await evaluate(`window.WebTransport = undefined; settings.bitrate.value = ${JSON.stringify(process.env.KBPS ?? '2000')};
    if (!settings.bitrate.value) throw Error('Choose a fixed bitrate supported by the settings menu');
    settings.size.value = 'auto'; settings.cursor.value = 'no'; settings.fps.value = '60'; settings.sound.value = 'off';
    login.password.value = ${JSON.stringify(password)}; login.fullscreen.checked = false; login.requestSubmit(); true`);
  for (let tries = 0; tries < 100 && !await evaluate('connected() && canvas.width > 360'); tries++) await sleep(100);
  if (!await evaluate('connected()')) throw Error('Desktop connection failed');
  await sleep(1500);
  return evaluate(`(${measure.toString()})(${duration}, ${idle})`);
});

function percentile(values, share) {
  const sorted = values.filter(value => value !== null).sort((a,b) => a-b);
  if (!sorted.length) return null;
  return +sorted[Math.ceil(sorted.length * share) - 1].toFixed(1);
}
for (const phase of [result.scroll, result.drag]) {
  const gaps = phase.changes.map((t, i) => t - (i ? phase.changes[i-1] : 0));
  gaps.push(phase.elapsed - (phase.changes.at(-1) ?? 0));
  phase.updates = phase.changes.length;
  delete phase.changes;
  Object.assign(phase, {p50: percentile(gaps,.5), p95: percentile(gaps,.95), p99: percentile(gaps,.99),
                        worst: percentile(gaps,1), over50: gaps.filter(gap => gap > 50).length});
}
console.log(JSON.stringify({...result, clickP50: percentile(result.click,.5), clickP95: percentile(result.click,.95)}));
if (result.scroll.updates < 4 || result.drag.updates < 4 || result.click.some(delay => delay === null))
  throw Error('Fixture did not respond to the actions');

// Runs inside the real browser client.
async function measure(seconds, idleSeconds) {
  const ws = socket, wait = ms => new Promise(done => setTimeout(done, ms));
  const move = (x,y) => moveTo(Math.round(x * POINTER_RANGE),Math.round(y * POINTER_RANGE));
  const reset = async () => { key(KEYS.Home,true); key(KEYS.Home,false); await wait(500); };
  move(.8,.75);
  await wait(150);
  key(BUTTONS[0],true); key(BUTTONS[0],false);
  await wait(150);
  const sample = kind => {
    const w = canvas.width, h = canvas.height;
    if (kind === 'click') return context.getImageData(Math.floor(w*.95),Math.floor(h*.92),1,1).data[0] > 128;
    const y = kind === 'scroll' ? Math.floor(h*.51) : Math.floor(h*.19);
    const pixels = context.getImageData(0,y,w,1).data;
    for (let x = 0; x < w; x++) {
      const i = x * 4, r = pixels[i], g = pixels[i+1], b = pixels[i+2];
      if (kind === 'scroll' ? g > 165 && r < 85 && b < 90 : r > 165 && b > 165 && g < 85)
        return Math.round(x / w * 1000);
    }
    return null;
  };
  async function motion(kind) {
    await reset();
    move(kind === 'scroll' ? .8 : .18, kind === 'scroll' ? .75 : .115);
    await wait(250);
    const startPosition = sample(kind);
    if (startPosition == null) throw Error('Missing '+kind+' marker at '+canvas.width+'x'+canvas.height);
    let last = startPosition, refresh, inputs = 0, draws = 0;
    const changes = [], sizes = new Set(), original = context.drawImage, start = performance.now();
    context.drawImage = function(...args) { draws++; return original.apply(this,args); };
    const observe = () => {
      const pos = sample(kind);
      if (pos !== null && pos !== last) { changes.push(performance.now()-start); last = pos; }
      sizes.add(canvas.width+'x'+canvas.height);
      refresh = requestAnimationFrame(observe);
    };
    if (kind === 'drag') key(BUTTONS[0],true);
    try {
      observe();
      const end = start + seconds * 1000;
      for (let due=start; performance.now()<end; due+=1000/60) {
        if (kind === 'scroll') send('scroll',0,-11);
        else {
          const cycle = ((performance.now()-start) % 1600) / 1600;
          move(.18 + .51 * (1-Math.abs(2*cycle-1)), .115);
        }
        inputs++;
        if (due < performance.now()-20) due = performance.now();
        await wait(Math.max(0,due+1000/60-performance.now()));
      }
    } finally {
      if (kind === 'drag') key(BUTTONS[0],false);
      cancelAnimationFrame(refresh);
      context.drawImage = original;
    }
    return {changes, elapsed: Math.round(performance.now()-start), inputs, draws, sizes:[...sizes]};
  }
  const scroll = await motion('scroll'), drag = await motion('drag');
  const idle = [];
  if (idleSeconds) {
    const start = performance.now();
    let previous;
    do {
      const size = canvas.width+'x'+canvas.height;
      if (previous !== size) idle.push({ms: Math.round(performance.now()-start), size});
      previous = size;
      await wait(50);
    } while (performance.now()-start < idleSeconds*1000);
  }
  await reset(); move(.8,.8); await wait(200);
  const click = [];
  for (let i=0; i<12; i++) {
    const old = sample('click'), start = performance.now();
    key(BUTTONS[0],true); key(BUTTONS[0],false);
    while (sample('click') === old && performance.now()-start < 1100)
      await new Promise(requestAnimationFrame);
    click.push(sample('click') === old ? null : Math.round(performance.now()-start));
    await wait(75 + i%5*3);
  }
  if (ws !== socket || ws.readyState !== WebSocket.OPEN) throw Error('Session changed during measurement');
  return {scroll, drag, idle, click, picture: $('now-picture').textContent, rate: $('now-bitrate').textContent};
}
