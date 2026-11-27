// Whether the desktop's own pointer is in the picture, on a real connection: the page moves the remote pointer
// to a place and away again and compares the picture there, round after round, so that something can be done
// to the desktop meanwhile. A pointer that the host paints, or one the desktop failed to leave out, changes
// some 150 pixels; with the page drawing the pointer itself there should be none.
// Arguments: the page's address, the Pointer setting (no or yes), the Size setting, how many rounds.
import { sleep, withPage } from './browser.mjs';

const [address, cursor = 'no', size = 'host', rounds = '1'] = process.argv.slice(2);
await withPage(address, async ({ evaluate }) => {
  await evaluate(`settings.cursor.value = '${cursor}'; settings.size.value = '${size}'; login.fullscreen.checked = false; login.requestSubmit(); true`);
  await sleep(6000);  // the picture is there
  for (let round = 0; round < +rounds; round++) {
    console.log(new Date().toISOString().slice(11, 19), await evaluate(`(async () => {
      const SIDE = 64, pause = ms => new Promise(done => setTimeout(done, ms));
      const places = [[0.25, 0.3], [0.6, 0.65]];
      const patch = ([x, y]) => context.getImageData(Math.round(x * canvas.width) - SIDE / 2, Math.round(y * canvas.height) - SIDE / 2, SIDE, SIDE).data;
      const differing = (a, b) => { let count = 0; for (let i = 0; i < a.length; i += 4) if (Math.abs(a[i] - b[i]) + Math.abs(a[i + 1] - b[i + 1]) + Math.abs(a[i + 2] - b[i + 2]) > 60) count++; return count; };
      const move = ([x, y]) => moveTo(Math.round(x * POINTER_RANGE), Math.round(y * POINTER_RANGE));
      const changed = [];
      for (let turn = 0; turn < 4; turn++) {
        const here = places[turn % 2], there = places[(turn + 1) % 2];
        move(there);
        await pause(600);
        const without = patch(here);
        move(here);
        await pause(600);
        changed.push(differing(patch(here), without));
      }
      return 'picture ' + canvas.width + 'x' + canvas.height + ', pixels changed where the pointer is: ' + changed.join(' ');
    })()`));
  }
});
