// Opens the client page in headless Chrome, connects, and prints once a second what the pill and panel show.
// Arguments: the page's address, seconds to watch, then optional JavaScript to run in the page at given seconds
// as "second:code" pairs. With SHOT naming a file, the picture shown in the last second is saved there as JPEG;
// with PAGE naming one, the whole page as the browser shows it then, panel and all.
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [address, seconds, ...actions] = process.argv.slice(2);
const port = 9333;
// CHROME names another browser binary than the Mac's Chrome; CHROME_FLAGS adds flags, such as --no-sandbox in a container.
const chrome = spawn(process.env.CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
  ...(process.env.CHROME_FLAGS?.split(' ') ?? []), '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${mkdtempSync(join(tmpdir(), 'page-check-'))}`,
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
  waiting.set(++serial, result => done(result?.result?.value ?? result?.exceptionDetails?.text));
  socket.send(JSON.stringify({ id: serial, method: 'Runtime.evaluate', params: { expression, returnByValue: true, userGesture: true } }));
});

await sleep(1500);
console.log('page:', await evaluate('document.title + " | " + location.href'));
console.log('connect:', await evaluate('login.fullscreen.checked = false; login.requestSubmit(); "submitted"'));
for (let second = 1; second <= +seconds; second++) {
  await sleep(1000);
  for (const action of actions) {
    const [at, ...code] = action.split(':');
    if (+at === second) console.log(`  at ${second}: ${code.join(':')} ->`, await evaluate(code.join(':')));
  }
  if (second === +seconds && process.env.SHOT) {
    const shot = await evaluate(`canvas.toDataURL('image/jpeg', 0.8)`);
    writeFileSync(process.env.SHOT, Buffer.from(shot.split(',')[1], 'base64'));
  }
  if (second === +seconds && process.env.PAGE) {
    const shot = await new Promise(done => {
      waiting.set(++serial, done);
      socket.send(JSON.stringify({ id: serial, method: 'Page.captureScreenshot', params: { format: 'jpeg', quality: 85 } }));
    });
    writeFileSync(process.env.PAGE, Buffer.from(shot.data, 'base64'));
  }
  console.log(String(second).padStart(3), await evaluate(`[
    document.body.classList.contains('connected') ? 'connected' : 'not connected',
    $('pill-text').textContent, 'bitrate ' + $('now-bitrate').textContent, $('now-fps').textContent, $('now-delay').textContent,
    document.body.classList.contains('slow') ? 'SLOW: ' + $('notice').textContent : '', $('now-picture').textContent,
    settings.screen.parentElement.hidden ? 'no screen choice' : 'screens ' + [...settings.screen.options].map(option => option.value) + ' -> ' + settings.screen.value,
    speaker ? 'sound ' + Math.round((playAt - speaker.currentTime) * 1000) + ' ms ahead' : 'no sound',
  ].join(' | ')`));
}
chrome.kill();
process.exit(0);
