// Watches the stream through the client page in headless Chrome and tells how late frames were drawn: each
// frame carries the time the host finished encoding it, so the time from there to the picture, less the same
// for the run's quickest frame, is what the way in between added. Host and browser must share a clock, as two
// containers on one machine do. Browser refresh callbacks sample which draws can reach distinct refreshes.
// Arguments: the page's address, seconds to watch.
import { sleep, withPage } from './browser.mjs';

const [address, seconds = '20'] = process.argv.slice(2);

const measured = await withPage(address, async ({ evaluate }) => {
  // SET holds JavaScript that chooses settings before the session begins, such as settings.bitrate.value = "2000".
  await evaluate(`${process.env.SET ?? ''}; showChoices(); login.fullscreen.checked = false; login.requestSubmit(); 'connecting'`);
  await sleep(6000);  // the picture is there and the bitrate has found its level

  const result = await evaluate(`(async () => {
    const draw = context.drawImage.bind(context), took = [], drawn = [], refreshed = [];
    let seen = 0, request;
    function refresh(time) {
      if (drawn.length !== seen) refreshed.push(time);
      seen = drawn.length;
      request = requestAnimationFrame(refresh);
    }
    request = requestAnimationFrame(refresh);
    context.drawImage = (frame, ...place) => {
      draw(frame, ...place);
      drawn.push(performance.now());
      took.push(performance.timeOrigin + performance.now() - frame.timestamp / 1000);
    };
    const start = performance.now();
    await new Promise(done => setTimeout(done, ${+seconds * 1000}));
    cancelAnimationFrame(request);
    context.drawImage = draw;
    return { took, drawn, refreshed, elapsed: performance.now() - start, bitrate: $('now-bitrate').textContent, picture: $('now-picture').textContent };
  })()`);
  // TELL holds a JavaScript expression whose value is printed at the end, for what SET set up to count.
  if (process.env.TELL) console.log(await evaluate(process.env.TELL));
  return result;
});

const { took, drawn } = measured;
if (took.length < 2) { console.log('no frames were drawn'); process.exit(1); }
const quickest = Math.min(...took);
const late = took.map(time => time - quickest).sort((a, b) => a - b);
const at = share => late[Math.min(late.length - 1, Math.floor(share * late.length))].toFixed(0).padStart(4);
const over = limit => (100 * late.filter(time => time > limit).length / late.length).toFixed(1).padStart(5);
function gaps(times) {
  if (times.length < 2) return 'unavailable';
  const sorted = times.slice(1).map((time, index) => time - times[index]).sort((a, b) => a - b);
  return [0.5, 0.95, 0.99, 1].map(p => sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))].toFixed(1)).join('/');
}
// How far each frame's distance to the one before it, as drawn, is from that distance as the host made them.
const uneven = took.slice(1).map((time, index) => Math.abs(time - took[index])).sort((a, b) => a - b);
const pace = share => uneven[Math.min(uneven.length - 1, Math.floor(share * uneven.length))].toFixed(0);
console.log(`${(1000 * took.length / measured.elapsed).toFixed(0).padStart(3)} fps at ${measured.bitrate} (${measured.picture.split(', ').slice(1).join(', ')}) | late by: half ${at(0.5)} ms, 9 in 10 within ${at(0.9)}, 99 in 100 within ${at(0.99)}, worst ${at(1)} | over 50 ms ${over(50)}%, over 100 ms ${over(100)}% | pace off by: half ${pace(0.5)} ms, 9 in 10 within ${pace(0.9)} | gaps p50/p95/p99/max (ms): draw ${gaps(drawn)}, refresh with new frame ${gaps(measured.refreshed)}`);
// With DETAIL set: second by second, how many frames were drawn and how late the latest of them was.
if (process.env.DETAIL) {
  const seconds = new Map();
  took.forEach((time, index) => {
    const second = Math.floor((drawn[index] - drawn[0]) / 1000), so = seconds.get(second) ?? [0, 0];
    seconds.set(second, [so[0] + 1, Math.max(so[1], time - quickest)]);
  });
  console.log([...seconds].map(([second, [frames, worst]]) => `${second}s: ${frames} frames, latest ${worst.toFixed(0)} ms`).join(' | '));
}
