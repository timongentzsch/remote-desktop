// Measures the delay of the whole loop on a real connection: the page moves the remote pointer and times how
// long it takes until a decoded picture shows the pointer at the new place. That covers the way up, the input,
// the desktop drawing it, capture, encoding, the way down and decoding; not the two displays themselves.
// The host has to paint the pointer into the picture for this, so the session is opened that way.
// Arguments: the page's address, how many moves to time (default 40).
import { spawn } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [address, moves = '40'] = process.argv.slice(2);
const port = 9334;
// CHROME names another browser binary than the Mac's Chrome; CHROME_FLAGS adds flags, such as --no-sandbox in a container.
const chrome = spawn(process.env.CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
  ...(process.env.CHROME_FLAGS?.split(' ') ?? []), '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${mkdtempSync(join(tmpdir(), 'delay-check-'))}`,
  '--ignore-certificate-errors', '--mute-audio', '--autoplay-policy=no-user-gesture-required', '--window-size=1280,800', address,
], { stdio: 'ignore' });
const sleep = ms => new Promise(done => setTimeout(done, ms));

let target;
for (let tries = 0; !target && tries < 50; tries++) {
  await sleep(200);
  try {
    target = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find(page => page.type === 'page');
  } catch {}
}
const socket = new WebSocket(target.webSocketDebuggerUrl);
await new Promise(open => socket.onopen = open);
let serial = 0;
const waiting = new Map();
socket.onmessage = event => {
  const message = JSON.parse(event.data);
  waiting.get(message.id)?.(message.result);
  waiting.delete(message.id);
};
const evaluate = expression => new Promise(done => {
  waiting.set(++serial, result => done(result?.result?.value ?? result?.exceptionDetails?.exception?.description ?? result?.exceptionDetails?.text));
  socket.send(JSON.stringify({ id: serial, method: 'Runtime.evaluate', params: { expression, returnByValue: true, awaitPromise: true } }));
});

await sleep(1500);
console.log(await evaluate(`settings.cursor.value = 'yes'; login.fullscreen.checked = false; login.requestSubmit(); 'connecting, the host paints the pointer'`));
await sleep(5000);  // the picture is there and the bitrate has found its level

// In the page: every picture that is drawn is looked at where the pointer is expected.
const result = await evaluate(`(async () => {
  const SIDE = 64, pause = ms => new Promise(done => setTimeout(done, ms));
  const places = [[0.25, 0.3], [0.6, 0.65]];
  const patch = ([x, y]) => context.getImageData(Math.round(x * canvas.width) - SIDE / 2, Math.round(y * canvas.height) - SIDE / 2, SIDE, SIDE).data;
  const differs = (a, b) => { let count = 0; for (let i = 0; i < a.length; i += 4) if (Math.abs(a[i] - b[i]) + Math.abs(a[i + 1] - b[i + 1]) + Math.abs(a[i + 2] - b[i + 2]) > 60) count++; return count > 12; };
  let drawn = () => {};
  const draw = context.drawImage.bind(context);
  context.drawImage = (...picture) => { draw(...picture); drawn(); };
  const move = ([x, y]) => send('move', Math.round(x * 65535), Math.round(y * 65535));
  move(places[1]);
  await pause(600);
  const delays = [];
  for (let turn = 0; turn < ${+moves}; turn++) {
    const place = places[turn % 2], before = patch(place), began = performance.now();
    const arrived = new Promise(done => { drawn = () => differs(patch(place), before) && done(performance.now() - began); });
    move(place);
    delays.push(await Promise.race([arrived, pause(2000).then(() => null)]));
    drawn = () => {};
    await pause(350 + Math.random() * 100);
  }
  const text = id => $(id).textContent;
  const parts = ['encode', 'send', 'network', 'decode'].map(name => name + ' ' + settings.querySelector('output.' + name).textContent);
  return JSON.stringify({ delays, picture: text('now-picture'), bitrate: text('now-bitrate'), fps: text('now-fps'), parts });
})()`);
chrome.kill();

let measured;
try { measured = JSON.parse(result); } catch { console.log('the measurement failed:', result); process.exit(1); }
const seen = measured.delays.filter(delay => delay !== null).sort((a, b) => a - b);
console.log(`picture ${measured.picture}, bitrate ${measured.bitrate} Mbit/s, ${measured.fps}; the page's own figures: ${measured.parts.join(', ')}`);
if (!seen.length) {
  console.log(`none of ${measured.delays.length} moves showed in the picture: the pointer is not visible on this screen, or not painted`);
} else {
  const at = share => seen[Math.min(seen.length - 1, Math.floor(share * seen.length))].toFixed(0);
  console.log(`pointer moved to picture shown, ${seen.length} of ${measured.delays.length} moves seen: fastest ${at(0)} ms, median ${at(0.5)} ms, nine in ten within ${at(0.9)} ms, slowest ${at(1)} ms`);
}
process.exit(0);
