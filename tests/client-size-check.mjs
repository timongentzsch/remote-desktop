// Resize one Client session, then check the original desktop from a fresh browser after disconnect.
import { readFileSync } from 'node:fs';
import { sleep, withPage } from './browser.mjs';

const [address, ...args] = process.argv.slice(2);
if (!address || !args.length) throw Error('Expected an HTTPS address and CSSWxH@ratio=displayWxH[:dpi] steps');
const steps = args.map(arg => {
  const match = /^(\d+)x(\d+)@(\d+(?:\.\d+)?)=(\d+)x(\d+)(?::(\d+))?$/.exec(arg);
  if (!match) throw Error('Invalid resize '+arg);
  const [width, height, deviceScaleFactor, goalWidth, goalHeight, dpi] = match.slice(1).map(Number);
  return {arg, metrics: {width, height, deviceScaleFactor, mobile: false}, expected: [goalWidth, goalHeight, dpi]};
});
const checkDpi = steps.some(step => Number.isFinite(step.expected[2]));
if (checkDpi && steps.some(step => !Number.isFinite(step.expected[2]))) throw Error('Specify Windows DPI for each step');
const password = process.env.PASSWORD_FILE ? readFileSync(process.env.PASSWORD_FILE, 'utf8').trimEnd() : '';
async function login(evaluate, size) {
  await evaluate(`settings.size.value=${JSON.stringify(size)}; login.password.value=${JSON.stringify(password)};
    login.fullscreen.checked=false; login.requestSubmit(); true`);
}

const original = await withPage(address, async ({evaluate}) => {
  await login(evaluate, 'host');
  return waitForSize(evaluate);
});
console.log('Initial desktop', original);
try {
  await withPage(address, async ({command, evaluate}) => {
    // Observe what went over the socket even when the driver clamps two requests to the same mode.
    await evaluate(`const sendMessage = WebSocket.prototype.send;
      WebSocket.prototype.send = function(message) {
        if (typeof message === 'string' && (message[0] === '{' || message.startsWith('["window",'))) {
          const packet = JSON.parse(message);
          if (!window.lastResize) this.addEventListener('message', ({data}) => {
            if (typeof data === 'string' && JSON.parse(data)[0] === 'scale') window.scaleReply = true;
          });
          window.scaleReply = false;
          window.lastResize = packet.window ?? packet[1];
        }
        return sendMessage.call(this, message);
      }; true`);
    for (const [index, {arg, metrics, expected}] of steps.entries()) {
      await command('Emulation.setDeviceMetricsOverride', metrics);
      if (index === 0) await login(evaluate, 'client');
      const wanted = JSON.stringify([metrics.width, metrics.height, metrics.deviceScaleFactor]);
      const deadline = Date.now() + 5000;
      while (await evaluate('JSON.stringify(window.lastResize)') !== wanted) {
        if (Date.now() >= deadline) throw Error('Browser did not send viewport '+wanted);
        await sleep(100);
      }
      console.log(arg, await waitForSize(evaluate, expected, checkDpi));
    }
  });
} finally {
  // The Client browser has exited, including on assertion failure. Host mode only observes the result.
  await withPage(address, async ({evaluate}) => {
    await login(evaluate, 'host');
    console.log('Restored after disconnect', await waitForSize(evaluate, original));
  });
}

async function waitForSize(evaluate, expected, checkScale = false) {
  const deadline = Date.now()+10000;
  let last;
  do {
    await sleep(100);
    last = await evaluate(`connected() && sourceSize !== canvas &&
      [sourceSize.width, sourceSize.height, canvas.width, canvas.height,
      ${checkDpi ? '('+markerWidth.toString()+')()' : 'null'}, ${checkScale ? 'window.scaleReply ? windowsScale : null' : 'null'}]`);
    if (!last) continue;
    const [width, height, videoWidth, videoHeight, dpi, scale] = last;
    if (width === videoWidth && height === videoHeight && (!checkDpi || dpi !== null) &&
        (!checkScale || scale === Math.round(expected[2] * 100 / 96)) &&
        (!expected || width === expected[0] && height === expected[1] && (!checkDpi || Math.abs(dpi-expected[2]) <= 2)))
      return checkDpi ? [width, height, dpi] : [width, height];
  } while (Date.now() < deadline);
  throw Error(`Expected ${expected ?? 'initial desktop'}, observed [display W,H, video W,H, DPI, Windows scale %] = ${last}`);
}

// dpi-window.ps1 paints a green bar between two magenta caps on the primary display.
function markerWidth() {
  const {data} = context.getImageData(0, 36, Math.min(550, canvas.width), 1);
  const magenta = x => data[4*x] > 150 && data[4*x+1] < 110 && data[4*x+2] > 150;
  if (!magenta(30)) return null;
  let width = 0;
  for (let x = 32; x < data.length/4; x++) {
    if (data[4*x+1] > 150 && data[4*x] < 110 && data[4*x+2] < 110) width++;
    if (width && magenta(x)) return width;
  }
  return null;
}
