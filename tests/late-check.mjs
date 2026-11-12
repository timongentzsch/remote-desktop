// Watches the stream through the client page in headless Chrome and tells how late frames were drawn: each
// frame carries the time the host finished encoding it, so the time from there to the picture, less the same
// for the run's quickest frame, is what the way in between added. Host and browser must share a clock, as two
// containers on one machine do.
// Arguments: the page's address, seconds to watch.
import { spawn } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [address, seconds = '20'] = process.argv.slice(2);
const port = 9335;
// CHROME names another browser binary than the Mac's Chrome; CHROME_FLAGS adds flags, such as --no-sandbox in a container.
const chrome = spawn(process.env.CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
  ...(process.env.CHROME_FLAGS?.split(' ') ?? []), '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${mkdtempSync(join(tmpdir(), 'late-check-'))}`,
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
// SET holds JavaScript that chooses settings before the session begins, such as settings.bitrate.value = "2000".
await evaluate(`${process.env.SET ?? ''}; showChoices(); login.fullscreen.checked = false; login.requestSubmit(); 'connecting'`);
await sleep(6000);  // the picture is there and the bitrate has found its level

const result = await evaluate(`(async () => {
  const draw = context.drawImage.bind(context), took = [], drawn = [];
  context.drawImage = (frame, ...place) => {
    draw(frame, ...place);
    drawn.push(performance.now());
    took.push(performance.timeOrigin + performance.now() - frame.timestamp / 1000);
  };
  await new Promise(done => setTimeout(done, ${+seconds * 1000}));
  return JSON.stringify({ took, drawn, bitrate: $('now-bitrate').textContent, picture: $('now-picture').textContent });
})()`);
// TELL holds a JavaScript expression whose value is printed at the end, for what SET set up to count.
if (process.env.TELL) console.log(await evaluate(process.env.TELL));
chrome.kill();

let measured;
try { measured = JSON.parse(result); } catch { console.log('the measurement failed:', result); process.exit(1); }
const { took, drawn } = measured;
if (took.length < 2) { console.log('no frames were drawn'); process.exit(1); }
const quickest = Math.min(...took);
const late = took.map(time => time - quickest).sort((a, b) => a - b);
const at = share => late[Math.min(late.length - 1, Math.floor(share * late.length))].toFixed(0).padStart(4);
const over = limit => (100 * late.filter(time => time > limit).length / late.length).toFixed(1).padStart(5);
const gap = Math.max(...drawn.slice(1).map((time, index) => time - drawn[index]));
// How far each frame's distance to the one before it, as drawn, is from that distance as the host made them.
const uneven = took.slice(1).map((time, index) => Math.abs(time - took[index])).sort((a, b) => a - b);
const pace = share => uneven[Math.min(uneven.length - 1, Math.floor(share * uneven.length))].toFixed(0);
console.log(`${(took.length / +seconds).toFixed(0).padStart(3)} fps at ${measured.bitrate} (${measured.picture.split(', ').slice(1).join(', ')}) | late by: half ${at(0.5)} ms, 9 in 10 within ${at(0.9)}, 99 in 100 within ${at(0.99)}, worst ${at(1)} | over 50 ms ${over(50)}%, over 100 ms ${over(100)}% | pace off by: half ${pace(0.5)} ms, 9 in 10 within ${pace(0.9)} | longest gap ${gap.toFixed(0)} ms`);
// With DETAIL set: second by second, how many frames were drawn and how late the latest of them was.
if (process.env.DETAIL) {
  const seconds = new Map();
  took.forEach((time, index) => {
    const second = Math.floor((drawn[index] - drawn[0]) / 1000), so = seconds.get(second) ?? [0, 0];
    seconds.set(second, [so[0] + 1, Math.max(so[1], time - quickest)]);
  });
  console.log([...seconds].map(([second, [frames, worst]]) => `${second}s: ${frames} frames, latest ${worst.toFixed(0)} ms`).join(' | '));
}
process.exit(0);
