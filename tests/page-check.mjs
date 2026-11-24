// Opens the client page in headless Chrome, connects, and prints once a second what the pill and panel show.
// Arguments: the page's address, seconds to watch, then optional JavaScript to run in the page at given seconds
// as "second:code" pairs. With SHOT naming a file, the picture shown in the last second is saved there as JPEG;
// with PAGE naming one, the whole page as the browser shows it then, panel and all.
import { sleep, withPage } from './browser.mjs';
import { writeFileSync } from 'node:fs';

const [address, seconds, ...actions] = process.argv.slice(2);

await withPage(address, async ({ command, evaluate }) => {
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
      const shot = await command('Page.captureScreenshot', { format: 'jpeg', quality: 85 });
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
}, { userGesture: true });
