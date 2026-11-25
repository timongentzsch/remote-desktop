// The page as a phone has it, emulated in headless Chrome: each gesture of the fingers, on the picture and on
// the trackpad, and what the page makes of it. The host is sent nothing of it: what the page would send is kept
// and looked at, so any host will do, and a password-less one is logged in to as it is.
//     node fingers-check.mjs https://HOST/
import { sleep, withPage } from './browser.mjs';
await withPage(process.argv[2], async ({ command, evaluate }) => {
  await command('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
  await command('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  await command('Page.reload');
  await sleep(1500);
  await evaluate(`login.fullscreen.checked=false; login.requestSubmit(); true`);
  await sleep(4000);
  await evaluate(`window.sent = []; socket.send = data => { typeof data === 'string' && !/^\\["(ping|decode)"/.test(data) && sent.push(data); }; true`);
  const pad = JSON.parse(await evaluate(`JSON.stringify((r => [r.left, r.top, r.width, r.height])($('pad').getBoundingClientRect()))`));
  const padY = pad[1] + pad[3] / 2, pictureY = 200;
  // The fingers that are down, as [x, y] or [x, y, which finger]; for touchEnd, those that lift, or none for all.
  const touch = (type, points) => command('Input.dispatchTouchEvent', { type, touchPoints: points.map(([x, y, id], index) => ({ x, y, id: id ?? index })) });
  const gesture = async (frames) => {
    await evaluate(`sent.length = 0; true`);
    await touch('touchStart', frames[0]);
    for (const frame of frames.slice(1)) { await touch('touchMove', frame); await sleep(12); }
    await touch('touchEnd', []);
    await sleep(120);
    return JSON.parse(await evaluate(`JSON.stringify({ zoom: Math.round(zoom * 100) / 100, page: visualViewport.scale, kinds: [...new Set(sent.map(m => JSON.parse(m)[0]))], scrolled: sent.filter(m => m.startsWith('["scroll"')).map(m => JSON.parse(m).slice(1)).reduce((a, b) => [a[0] + b[0], a[1] + b[1]], [0, 0]), keys: sent.filter(m => m.startsWith('["key"')).map(m => JSON.parse(m).slice(1).join(':')).slice(0, 4) })`));
  };
  const pinch = (y, from, to) => Array.from({ length: 12 }, (_, i) => { const s = from + (to - from) * i / 11; return [[195 - s, y], [195 + s, y]]; });
  const drag2 = (y, by) => Array.from({ length: 12 }, (_, i) => [[150, y + by * i / 11], [240, y + by * i / 11]]);
  const drag1 = (y, by) => Array.from({ length: 12 }, (_, i) => [[150 + by * i / 11, y]]);
  const say = (what, result, good) => { console.log(good ? 'ok  ' : 'FAIL', what, JSON.stringify(result)); if (!good) process.exitCode = 1; };
  let r;
  r = JSON.parse(await evaluate(`JSON.stringify([getComputedStyle(document.body).touchAction, getComputedStyle($('pad')).userSelect, getComputedStyle(document.querySelector('.typing')).userSelect])`));
  say('the browser neither zooms the page nor marks its text, and the field typed into takes text', r, r.join() === 'none,none,text');
  r = await gesture(pinch(pictureY, 30, 100)); say('picture, pinch out: zooms here, sends nothing', r, r.zoom > 2 && !r.kinds.length && r.page === 1);
  const zoomed = r.zoom;
  r = await gesture(drag2(pictureY, 120)); say('picture, two fingers dragged: nothing', r, r.zoom === zoomed && !r.kinds.length);
  r = await gesture(drag1(pictureY, 150)); say('picture, one finger dragged: nothing', r, !r.kinds.length);
  r = await gesture([[[150, pictureY]]]); say('picture, tap: nothing', r, !r.kinds.length);
  r = await gesture(pinch(padY, 30, 62)); say('trackpad, pinch out: Ctrl held, scrolls up, no zoom here', r, r.zoom === zoomed && r.scrolled[1] > 300 && r.keys[0] === '29:1' && r.keys[1] === '29:0');
  r = await gesture(pinch(padY, 62, 30)); say('trackpad, pinch in: scrolls down', r, r.scrolled[1] < -300);
  r = await gesture(drag2(padY, 60)); say('trackpad, two fingers dragged: scrolls, no Ctrl', r, r.scrolled[1] !== 0 && !r.keys.length && r.zoom === zoomed);
  r = await gesture(drag1(padY, 100)); say('trackpad, one finger dragged: moves the pointer', r, r.kinds.join() === 'move');
  r = await gesture([[[150, padY]]]); say('trackpad, tap: a click', r, r.keys.join() === '272:1,272:0');
  r = await gesture([[[150, padY], [240, padY]]]); say('trackpad, two-finger tap: nothing', r, !r.kinds.length);
  // A second finger that joins one already resting, and both moved: a scroll all the same.
  await evaluate(`sent.length = 0; true`);
  await touch('touchStart', [[150, padY]]);
  await sleep(400);
  await touch('touchStart', [[150, padY], [240, padY]]);
  for (let y = padY; y <= padY + 60; y += 6) await touch('touchMove', [[150, y], [240, y]]);
  await touch('touchEnd', []);
  await sleep(120);
  r = JSON.parse(await evaluate(`JSON.stringify([...new Set(sent.map(m => JSON.parse(m)[0]))])`));
  say('trackpad, a finger joins a resting one and both move: scrolls, no button', r, r.join() === 'scroll');
  // The buttons along the trackpad's bottom edge: down while a finger is on them.
  const [left, right] = JSON.parse(await evaluate(`JSON.stringify([...document.querySelectorAll('[data-button]')].map(b => (r => [r.left + r.width / 2, r.top + r.height / 2])(b.getBoundingClientRect())))`));
  const keys = async () => JSON.parse(await evaluate(`JSON.stringify(sent.map(m => JSON.parse(m)).map(m => m[0] === 'key' ? m.slice(1).join(':') : m[0]))`));
  await evaluate(`sent.length = 0; true`);
  await touch('touchStart', [[...left, 0]]);
  r = await keys(); say('Left button, finger down: the button is down', r, r.join() === '272:1');
  await touch('touchStart', [[...left, 0], [200, padY, 1]]);
  for (let x = 200; x <= 300; x += 10) await touch('touchMove', [[...left, 0], [x, padY, 1]]);
  await touch('touchEnd', [[300, padY, 1]]);
  await sleep(120);
  r = [...new Set(await keys())]; say('Left button held, another finger moved and lifted: the pointer moves, the button stays down', r, r.join() === '272:1,move');
  await touch('touchStart', [[...left, 0], [200, padY, 1]]);
  await touch('touchEnd', [[200, padY, 1]]);
  await sleep(120);
  r = [...new Set(await keys())]; say('Left button held, a tap on the trackpad: no click of its own', r, r.join() === '272:1,move');
  await touch('touchEnd', []);
  await sleep(120);
  r = await keys(); say('Left button, finger up: the button is let go', r.slice(-1), r.at(-1) === '272:0' && r.filter(k => k === '272:0').length === 1);
  await evaluate(`sent.length = 0; true`);
  await touch('touchStart', [[...right, 0]]);
  await touch('touchEnd', []);
  await sleep(120);
  r = await keys(); say('Right button, pressed and let go: a right click', r, r.join() === '273:1,273:0');
  await sleep(600);
  // The page's own controls still take a tap: the pill opens the panel.
  const pill = JSON.parse(await evaluate(`JSON.stringify((r => [r.left + r.width / 2, r.top + r.height / 2])($('pill').getBoundingClientRect()))`));
  await touch('touchStart', [pill]);
  await touch('touchEnd', []);
  await sleep(300);
  r = await evaluate(`document.body.classList.contains('panel-open')`);
  say('a tap on the pill opens the panel', r, r === true);
  await touch('touchStart', [[150, padY]]);
  await touch('touchEnd', []);
  await sleep(300);
  // A finger whose end never came must not spoil the next gesture.
  await sleep(300);
  await evaluate(`fingers.set(99, { x: 5, y: 5 }); true`);
  r = await gesture(drag1(padY, 100)); say('trackpad, after a lost finger: still one finger moving the pointer', r, r.kinds.join() === 'move');
  // Three fingers, one lifted: the two that remain do not jump the zoom.
  await evaluate(`sent.length = 0; true`);
  await touch('touchStart', [[100, pictureY], [130, pictureY], [330, pictureY]]);
  await touch('touchMove', [[100, pictureY], [130, pictureY], [331, pictureY]]);
  await touch('touchEnd', [[130, pictureY, 1]]);
  await touch('touchMove', [[100, pictureY, 0], [332, pictureY, 2]]);
  const after = await evaluate(`Math.round(zoom * 100) / 100`);
  await touch('touchEnd', []);
  say('picture, three fingers then two, moved a pixel: the zoom does not jump', { zoom: after }, Math.abs(after - zoomed) < .05);
  // The arrow that stands in for the host's pointer is 24 of the host's pixels high, whatever the zoom.
  r = JSON.parse(await evaluate(`JSON.stringify([1, 4].map(times => { zoom = times; place(); return parseFloat(document.documentElement.style.getPropertyValue('--point-size')) / (canvas.getBoundingClientRect().width / sourceSize.width); }))`));
  say('the arrow is 24 of the host\'s pixels high at every zoom', r, r.every(height => Math.abs(height - 24) < .01));
});
