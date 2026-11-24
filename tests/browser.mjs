// Runs a check against its own headless Chrome. CHROME and CHROME_FLAGS select the browser in containers.
import { spawn } from 'node:child_process';
import { on, once } from 'node:events';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

export { sleep };

export async function withPage(address, check, evaluateOptions = { awaitPromise: true }) {
  const profile = mkdtempSync(join(tmpdir(), 'desktop-check-'));
  const chrome = spawn(process.env.CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
    ...(process.env.CHROME_FLAGS?.split(' ') ?? []), '--headless=new', '--remote-debugging-port=0',
    `--user-data-dir=${profile}`, '--ignore-certificate-errors', '--mute-audio',
    '--autoplay-policy=no-user-gesture-required', '--window-size=1280,800', address,
  ], { stdio: 'ignore' });
  const exited = new Promise(done => chrome.once('close', done));
  const stop = () => chrome.kill();
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
  let socket;
  try {
    await once(chrome, 'spawn');
    let page;
    for (let tries = 0; !page && tries < 50 && chrome.exitCode === null && !chrome.killed; tries++) {
      await sleep(200);
      try {
        const port = readFileSync(join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0];
        const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
        page = targets.find(target => target.type === 'page' && target.url && target.url !== 'about:blank');
      } catch {} // Chrome is still starting.
    }
    if (!page) throw new Error('Chrome did not open a debugging target');
    socket = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
    const messages = on(socket, 'message', { close: ['close'] });
    let serial = 0;
    // All checks issue one command at a time. Other messages are browser events.
    async function command(method, params) {
      const id = ++serial;
      socket.send(JSON.stringify({ id, method, params }));
      for (;;) {
        const { value, done } = await messages.next();
        if (done) throw new Error(`Chrome disconnected during ${method}`);
        const reply = JSON.parse(value[0].data);
        if (reply.id !== id) continue;
        if (reply.error) throw new Error(reply.error.message);
        return reply.result;
      }
    }
    async function evaluate(expression) {
      const { result, exceptionDetails } = await command('Runtime.evaluate', { expression, returnByValue: true, ...evaluateOptions });
      if (exceptionDetails) throw new Error(exceptionDetails.exception?.description ?? exceptionDetails.text);
      return result.value;
    }
    // DevTools can publish the target URL while its execution context is still about:blank.
    const deadline = Date.now() + 15000;
    while (!await evaluate("location.href !== 'about:blank' && document.readyState === 'complete'")) {
      if (Date.now() >= deadline) throw new Error(`Page did not finish loading: ${address}`);
      await sleep(100);
    }
    return await check({ command, evaluate });
  } finally {
    socket?.close();
    stop();
    await exited;
    rmSync(profile, { recursive: true, force: true, maxRetries: 3 });
    process.off('SIGINT', stop);
    process.off('SIGTERM', stop);
  }
}
