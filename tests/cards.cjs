// Generate real-registry fixtures first:
// HELP_CARD_DUMP=/tmp/cards/help CTL_CARD_DUMP=/tmp/cards/ctl cargo test renders_sample_cards_to_png -- --ignored --test-threads=1
// CARD_ARTIFACTS=/tmp/cards node tests/cards.cjs
// Also accepts ai_news and oai fixtures; writes screenshots and a layout report.
// Uses only local HTML fixtures and an isolated Chromium profile.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const {spawn} = require('node:child_process');
const {pathToFileURL} = require('node:url');
const artifacts = process.env.CARD_ARTIFACTS;
assert(artifacts, 'Set CARD_ARTIFACTS to the fixture directory');
let chrome, ws, sequence = 0;
const pending = new Map();
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'acumen-cards-'));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(fn) {
  const end = Date.now() + 20000;
  while (Date.now() < end) { try { if (await fn()) return; } catch {} await sleep(100); }
  throw Error('Browser did not become ready');
}
function cdp(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++sequence;
    const timer = setTimeout(() => { pending.delete(id); reject(Error('CDP timeout: ' + method)); }, 20000);
    pending.set(id, {resolve, reject, timer});
    ws.send(JSON.stringify({id, method, params}));
  });
}
async function run(expression) {
  const result = await cdp('Runtime.evaluate', {expression, returnByValue:true, awaitPromise:true});
  assert(!result.exceptionDetails, JSON.stringify(result.exceptionDetails));
  return result.result.value;
}
async function main() {
  const server = net.createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  const url = `http://127.0.0.1:${port}`;
  chrome = spawn(process.env.CHROME_BIN || 'chromium-browser', ['--headless', '--no-sandbox', '--disable-gpu',
    '--disable-dev-shm-usage', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, 'about:blank'], {stdio:'ignore'});
  chrome.on('error', error => { console.error(error.message); });
  await until(async () => (await fetch(url + '/json/version')).ok);
  const page = await (await fetch(url + '/json/new?about:blank', {method:'PUT'})).json();
  ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  ws.onmessage = event => {
    const message = JSON.parse(event.data), task = pending.get(message.id);
    if (!task) return;
    pending.delete(message.id); clearTimeout(task.timer);
    message.error ? task.reject(Error(message.error.message)) : task.resolve(message.result);
  };
  await cdp('Page.enable');
  await cdp('Emulation.setDeviceMetricsOverride', {width:640, height:800, deviceScaleFactor:1, mobile:false});
  const report = [];
  for (const family of (process.env.CARD_FAMILIES || 'help,ctl,ai_news,oai').split(',')) {
    const dir = path.join(artifacts, family);
    if (!fs.existsSync(dir)) { assert(!['help','ctl'].includes(family), family + ' fixtures missing'); continue; }
    const files = fs.readdirSync(dir).filter(file => file.endsWith('.html'));
    assert(files.length >= ({help:4,ctl:5,ai_news:5,oai:2}[family]));
    for (const file of files) {
      const viewport = {help:920,ctl:640,ai_news:720,oai:560}[family];
      await cdp('Emulation.setDeviceMetricsOverride', {width:viewport, height:800, deviceScaleFactor:1, mobile:false});
      await cdp('Page.navigate', {url:pathToFileURL(path.resolve(dir, file)).href});
      await until(() => run(`document.readyState === 'complete' && !!document.querySelector('.card')`));
      await run('document.fonts.ready.then(() => true)');
      const metrics = await run(`(() => {
        const shot = document.querySelector('.shot').getBoundingClientRect();
        const overflow = [...document.querySelectorAll('.card *')].filter(el => {
          const rect = el.getBoundingClientRect();
          return rect.width && (rect.left < shot.left || rect.right > shot.right + 1 ||
            (el.clientWidth && el.scrollWidth > el.clientWidth + 2));
        }).map(el => el.className || el.tagName);
        return {x:shot.x,y:shot.y,width:shot.width, height:shot.height, overflow, scriptRan:!!window.cardScriptRan,
          commands:document.querySelectorAll('.command').length,
          items:document.querySelectorAll('.item').length,
          states:document.querySelectorAll('.status-row').length};
      })()`);
      assert.equal(metrics.width, family === 'help' ? (file === 'overview.html' ? 920 : 640) : viewport, file);
      assert.equal(metrics.scriptRan, false, file + ': embedded script executed');
      assert.deepEqual(metrics.overflow, [], file + ': content overflow');
      assert(metrics.height <= 16000, file + ': excessive height');
      if (file === 'overview.html') assert(metrics.items >= 20);
      if (file === 'detail_widest.html') assert(metrics.commands >= 10);
      // Capture the complete local document.
      const capture = await cdp('Page.captureScreenshot', {format:'png', captureBeyondViewport:true, clip:{x:metrics.x,y:metrics.y,width:metrics.width,height:metrics.height,scale:1}});
      fs.writeFileSync(path.join(dir, file.replace('.html', '-review.png')), Buffer.from(capture.data, 'base64'));
      // Probe long unbroken values, aliases, escaping and CSS whitespace preservation in the browser.
      if (file === 'config.html') {
        await run(`document.querySelector('.code-line code').textContent = '  key = "' + 'LongValue中文'.repeat(100) + '"'; true`);
        assert.equal(await run(`document.querySelector('.code-line').scrollWidth <= document.querySelector('.code-line').clientWidth + 1`), true);
        assert.equal(await run(`getComputedStyle(document.querySelector('.code-line code')).whiteSpace`), 'pre-wrap');
      }
      report.push({file:family + '/' + file, ...metrics});
      console.log(`PASS ${family}/${file}: ${metrics.width} × ${metrics.height}, no overflow`);
    }
  }
  fs.writeFileSync(path.join(artifacts, 'layout-audit.json'), JSON.stringify(report, null, 2) + '\n');
  const pictures = report.map(item => item.file.replace('.html', '-review.png'));
  for (const family of ['stats', 'wordcloud']) {
    const dir = path.join(artifacts, family);
    if (fs.existsSync(dir)) pictures.push(...fs.readdirSync(dir).filter(file => file.endsWith('.png')).map(file => family + '/' + file));
  }
  const esc = value => value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('"', '&quot;');
  fs.writeFileSync(path.join(artifacts, 'index.html'), `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>ACUMEN 图片样张</title><style>body{margin:32px;background:#edf1ed;color:#253b37;font-family:system-ui,sans-serif}main{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,360px),1fr));gap:24px;align-items:start}figure{margin:0;padding:16px;background:#fffefa;border-radius:16px}figcaption{padding:0 0 12px;overflow-wrap:anywhere}img{width:100%;height:auto;display:block}a{color:inherit}</style><h1>ACUMEN 图片样张</h1><p>本地合成数据 · 点击图片查看完整尺寸</p><main>${pictures.map(file => `<figure><figcaption>${esc(file)}</figcaption><a href="${esc(file)}"><img loading="lazy" src="${esc(file)}"></a></figure>`).join('')}</main></html>`);

}
main().catch(error => { console.error(error); process.exitCode = 1; }).finally(async () => {
  for (const task of pending.values()) clearTimeout(task.timer);
  ws?.close();
  if (chrome && chrome.exitCode === null) {
    const exited = new Promise(resolve => chrome.once('exit', resolve));
    chrome.kill('SIGTERM');
    await Promise.race([exited, sleep(3000)]);
    if (chrome.exitCode === null) chrome.kill('SIGKILL');
  }
  fs.rmSync(profile, {recursive:true, force:true});
});
