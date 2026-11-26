// What the page does in a browser that does not decode what the host sends, with such browsers stood in for by
// headless Chrome and a decoder that fails where told. A host that sends H.264 with full colour detail is
// needed, which is one without a GPU's encoder, and a password-less one is logged in to as it is.
//     node decoder-check.mjs https://HOST/
import { sleep, withPage } from './browser.mjs';

// A decoder that gives up on the streams whose codec string `fails` says so of, as Apple's does on the High
// 4:4:4 Predictive profile, and counts the connections the page opens.
const standIn = fails => `
  const Decoder = VideoDecoder;
  window.VideoDecoder = class extends Decoder {
    constructor(callbacks) { super(callbacks); this.gaveUp = callbacks.error; }
    configure(configuration) {
      this.refused = (${fails})(configuration.codec);
      if (this.refused) queueMicrotask(() => this.gaveUp(new DOMException('stood in', 'NotSupportedError')));
      else super.configure(configuration);
    }
    decode(chunk) { if (!this.refused) super.decode(chunk); }
  };
  const Socket = WebSocket;
  window.opened = 0;
  window.WebSocket = class extends Socket { constructor(...address) { super(...address); opened++; } };
  true`;
const FULL = 'codec => codec.startsWith("avc1.f4")';
const browsers = [
  ['decodes everything', standIn('() => false'), [true, 'full', 1, '']],
  ['says it has no full colour detail', standIn('() => false') + '; decodesFull = Promise.resolve(false); true', [true, 'reduced', 1, '']],
  ['fails on full colour detail', standIn(FULL), [true, 'reduced', 2, '']],
  // Asked once more without full colour detail, then not again.
  ['fails on everything', standIn('() => true'), [false, '', 2, 'This browser cannot decode the video.']],
];
for (const [browser, prepared, expected] of browsers) {
  await withPage(process.argv[2], async ({ evaluate }) => {
    await evaluate(prepared);
    await evaluate(`login.fullscreen.checked = false; login.requestSubmit(); true`);
    await sleep(7000);
    const seen = JSON.parse(await evaluate(`JSON.stringify([
      document.body.classList.contains('connected') && canvas.width > 300,
      document.body.classList.contains('connected') ? colourDetail : '', opened, $('summary').textContent])`));
    const good = JSON.stringify(seen) === JSON.stringify(expected);
    console.log(good ? 'ok  ' : 'FAIL', browser, JSON.stringify(seen), good ? '' : `instead of ${JSON.stringify(expected)}`);
    if (!good) process.exitCode = 1;
  });
}
