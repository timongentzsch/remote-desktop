// Measures the delay of the whole loop on a real connection: the page moves the remote pointer and times how
// long it takes until a decoded picture shows the pointer at the new place. That covers the way up, the input,
// the desktop drawing it, capture, encoding, the way down and decoding; not the two displays themselves.
// The host has to paint the pointer into the picture for this, so the session is opened that way.
// Arguments: the page's address, how many moves to time (default 40).
import { sleep, withPage } from './browser.mjs';

const [address, moves = '40'] = process.argv.slice(2);

const measured = await withPage(address, async ({ evaluate }) => {
  console.log(await evaluate(`settings.cursor.value = 'yes'; login.fullscreen.checked = false; login.requestSubmit(); 'connecting, the host paints the pointer'`));
  await sleep(5000);  // the picture is there and the bitrate has found its level

  // In the page: every picture that is drawn is looked at where the pointer is expected.
  return evaluate(`(async () => {
    const SIDE = 64, pause = ms => new Promise(done => setTimeout(done, ms));
    const places = [[0.25, 0.3], [0.6, 0.65]];
    const patch = ([x, y]) => context.getImageData(Math.round(x * canvas.width) - SIDE / 2, Math.round(y * canvas.height) - SIDE / 2, SIDE, SIDE).data;
    const differs = (a, b) => { let count = 0; for (let i = 0; i < a.length; i += 4) if (Math.abs(a[i] - b[i]) + Math.abs(a[i + 1] - b[i + 1]) + Math.abs(a[i + 2] - b[i + 2]) > 60) count++; return count > 12; };
    let drawn = () => {};
    const draw = context.drawImage.bind(context);
    context.drawImage = (...picture) => { draw(...picture); drawn(); };
    const move = ([x, y]) => moveTo(Math.round(x * POINTER_RANGE), Math.round(y * POINTER_RANGE));
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
    return { delays, picture: text('now-picture'), bitrate: text('now-bitrate'), fps: text('now-fps'), parts };
  })()`);
});

const seen = measured.delays.filter(delay => delay !== null).sort((a, b) => a - b);
console.log(`picture ${measured.picture}, bitrate ${measured.bitrate}, ${measured.fps}; the page's own figures: ${measured.parts.join(', ')}`);
if (!seen.length) throw new Error(`none of ${measured.delays.length} moves showed in the picture`);
const at = share => seen[Math.min(seen.length - 1, Math.floor(share * seen.length))].toFixed(0);
console.log(`pointer moved to picture shown, ${seen.length} of ${measured.delays.length} moves seen: fastest ${at(0)} ms, median ${at(0.5)} ms, nine in ten within ${at(0.9)} ms, slowest ${at(1)} ms`);
