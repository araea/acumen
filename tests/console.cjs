// node tests/console.cjs — real Chromium, isolated HTTP fixture; never touches a running bot.
// Optional ACUMEN_CONSOLE_SHOTS exports screenshots and timing measurements.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const { spawn } = require('node:child_process');
const root = path.resolve(__dirname, '..');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const out = process.env.ACUMEN_CONSOLE_SHOTS;
if (out) fs.mkdirSync(out, { recursive: true });
const plugins = [
  ['logger', '日志', '记录运行日志'], ['oai', '智能对话', '与模型对话，管理房间与预设'],
  ['console', '控制台', '查看本机运行状态'], ['ambient', '搭话', '参与群聊，记住熟悉的人'],
].map(([name, display, summary], index) => ({ name, display, summary, section: 'system', on: index !== 2,
  pending: false, effect: '下一条消息生效', commands: [], configurable: true,
  config: { enabled: true, model: '示例模型', retries: 2 }, defaults: {}, diff: [] }));
const sections = [{ code: 'system', name: '系统' }];
const settings = { bots: [{ enabled: true, protocol: 'satori', url: 'http://127.0.0.1:3001', has_token: false }],
  command_prefix: ['/'], browser_path: '', global_filter: { enable_blacklist: false, blacklist: [], enable_whitelist: false, whitelist: [] } };
let posts = [], streams = new Set(), sequence = 0, detailDelay = false, rejectStreams = false;
const token = 'fixture';
const line = text => ({ at: '12:30:00', level: ['INFO','WARN','ERRO','DEBG'][sequence++ % 4], target: 'Plugin/Console', text });
let history = Array.from({ length: 80 }, (_, i) => line(`运行记录 ${i} · 已完成处理`));
function emit(count) {
  const lines = Array.from({ length: count }, () => line(`压力样本 ${sequence} <script>window.injected=true</script>`));
  history = history.concat(lines).slice(-2000);
  const batches=[];
  for(let i=0;i<lines.length;i+=128) batches.push(`event: batch\ndata: ${JSON.stringify({lines:lines.slice(i,i+128)})}\n\n`);
  const body = batches.join('');
  for (const response of streams) response.write(body);
}
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const reply = (data, status = 200) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(data)); };
  // 夹具也认口令：凭证发错（请求头或 ?t=）要当场变红，而不是静默放行。
  if (url.pathname.startsWith('/api')
      && req.headers['x-acumen-token'] !== token && url.searchParams.get('t') !== token) {
    return reply({ error: '口令不对' }, 401);
  }
  if (req.method === 'POST') {
    let data = ''; for await (const chunk of req) data += chunk;
    const body = JSON.parse(data); posts.push({ path: url.pathname, body });
    if (url.pathname.endsWith('/enabled')) plugins.find(p => url.pathname.includes(`/${p.name}/`)).on = body.on;
    await sleep(60); return reply({ message: '已保存 · ' + (body.input || '') });
  }
  if (url.pathname === '/api/overview') return reply({
    app: { version: '0.1.0', started: '2026-09-16 08:30:00', uptime: 8426 }, bots: [{ name: '知微', adapter: 'satori', platform: 'QQ', id: '10001' }],
    plugins: { on: 22, total: 23, pending: 0 }, messages: { today: 3803, people: 711, week: 84737 }, console: { address: 'http://127.0.0.1:7801/' },
  });
  if (url.pathname === '/api/plugins') return reply({ plugins, sections });
  if (url.pathname.startsWith('/api/plugins/')) {
    const plugin = plugins.find(p => url.pathname === `/api/plugins/${p.name}`);
    if (detailDelay && plugin?.name === 'oai') await sleep(300);
    return reply(plugin || {}, plugin ? 200 : 404);
  }
  if (url.pathname === '/api/settings') return reply(settings);
  if (url.pathname === '/api/ambient') return reply({ ready: true, persona: '自然参与群聊，先听懂，再开口。', self: '知微', memory: [], stickers: [{ id: 1, label: '示例表情包', image: true, uses: 1 }] });
  if (url.pathname === '/api/ambient/sticker/1') {
    res.writeHead(200, { 'Content-Type': 'image/png' });
    return res.end(Buffer.from('89504e470d0a1a0a', 'hex'));
  }
  if (url.pathname === '/api/logs') return reply({ lines: history.slice(-Number(url.searchParams.get('limit') || 2000)) });
  if (url.pathname === '/api/logs/stream') {
    if (rejectStreams) return reply({ error: 'restarting' }, 503);
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
    res.write(`event: snapshot\ndata: ${JSON.stringify({ lines: history })}\n\n`);
    streams.add(res); res.on('close', () => streams.delete(res)); return;
  }
  const assets = { '/': ['res/console/index.html','text/html'], '/app.js': ['res/console/app.js','text/javascript'], '/app.css': ['res/console/app.css','text/css'], '/icon.svg': ['res/console/icon.svg','image/svg+xml'], '/manifest.webmanifest': ['res/console/manifest.webmanifest','application/manifest+json'] };
  if (!assets[url.pathname]) { res.writeHead(404); return res.end(); }
  const [file, type] = assets[url.pathname]; res.writeHead(200, { 'Content-Type': type });
  if (url.pathname === '/app.css') res.write(fs.readFileSync(path.join(root, 'res/cards/m3e.css')));
  res.end(fs.readFileSync(path.join(root, file)));
});
let session, driver;
const driverPort = Number(process.env.CHROMEDRIVER_PORT || 9529);
async function call(method, route, data) {
  const response = await fetch(`http://127.0.0.1:${driverPort}${route}`, { method,
    headers: { 'Content-Type': 'application/json' }, body: data === undefined ? undefined : JSON.stringify(data), signal: AbortSignal.timeout(30000) });
  const result = (await response.json()).value;
  if (result?.error) throw new Error(`${result.error}: ${result.message}`);
  return result;
}
const cmd = (method, route, data) => call(method, `/session/${session}${route}`, data);
const js = (script, ...args) => cmd('POST', '/execute/sync', { script, args });
async function until(predicate, name) {
  for (let i = 0; i < 100; i++) { if (await predicate()) return; await sleep(50); }
  throw new Error(`Timeout: ${name}`);
}
async function click(selector) {
  await js('document.querySelector(arguments[0]).scrollIntoView({block:"center"})', selector);
  await sleep(80);
  const node = await cmd('POST', '/element', { using: 'css selector', value: selector });
  await cmd('POST', `/element/${node['element-6066-11e4-a52e-4f735466cecf']}/click`, {});
}
async function route(name) {
  await js('location.hash = arguments[0]', '#/' + name);
  await until(() => js('return !document.querySelector("#view[aria-busy]") && !!document.querySelector(".page-head")'), name);
  await sleep(100);
}
const media = features => cmd('POST', '/goog/cdp/execute', { cmd: 'Emulation.setEmulatedMedia', params: { features } });
async function viewport(width, height) {
  await cmd('POST', '/goog/cdp/execute', { cmd: 'Emulation.setDeviceMetricsOverride', params: { width, height, deviceScaleFactor: 1, mobile: false } });
  await sleep(150);
}
async function shot(name) {
  if (!out) return;
  fs.writeFileSync(path.join(out, `${name}.png`), Buffer.from(await cmd('GET', '/screenshot'), 'base64'));
}
(async () => {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  driver = spawn('chromedriver', [`--port=${driverPort}`, '--log-level=SEVERE'], { stdio: 'ignore' });
  driver.on('error', error => console.error(error.message));
  await until(async () => { try { await call('GET', '/status'); return true; } catch { return false; } }, 'chromedriver');
  session = (await call('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'chrome',
    'goog:chromeOptions': { args: ['--headless=new', '--no-sandbox', '--disable-gpu'] }, 'goog:loggingPrefs': { browser: 'ALL' } } } })).sessionId;
  await viewport(390, 844);
  await cmd('POST', '/url', { url: `http://127.0.0.1:${server.address().port}/?t=fixture` });
  await until(() => js('return !!document.querySelector(".hero")'), 'initial view');
  for (const name of ['plugins','ambient','logs','command','overview']) {
    await click(`[data-nav="${name}"]`); await sleep(160);
    assert.equal(await js('return location.hash'), '#/' + name);
  }
  await viewport(1400, 900); await route('plugins');
  await click('[data-plugin=logger] .switch');
  await until(() => posts.some(p => p.path === '/api/plugins/logger/enabled'), 'wide switch POST');
  assert.equal(await js('return location.hash'), '#/plugins');
  await until(() => js('return document.querySelector("[data-plugin=logger] .switch")?.getAttribute("aria-checked") === "false"'), 'confirmed switch state');
  detailDelay = true;
  await js('document.querySelector("[data-plugin=oai] .row-hit").click(); document.querySelector("[data-plugin=console] .row-hit").click()');
  await sleep(500);
  assert.equal(await js('return location.hash'), '#/plugins/console', 'slow earlier detail must not win');
  assert.equal(await js('return document.querySelector("#plugin-detail .key").textContent'), 'console');
  await click('[data-reset]');
  assert.equal(await js('return document.querySelector("dialog").open'), true);
  assert.equal(await js('return document.querySelector("dialog").getAttribute("aria-labelledby")'), 'dialog-title');
  await click('[data-answer=no]');
  await route('command');
  await js('document.querySelector("#command-input").value="list"');
  await click('#command-form [type=submit]');
  await until(() => js('return document.querySelector("#command-output").textContent.includes("已保存")'), 'submit command');
  assert.equal(posts.filter(p => p.path === '/api/command').length, 1);
  assert.equal(await js('return location.hash'), '#/command');
  const commandInput = await cmd('POST','/element',{using:'css selector',value:'#command-input'});
  await cmd('POST',`/element/${commandInput['element-6066-11e4-a52e-4f735466cecf']}/value`,{text:'\uE007'});
  await until(()=>posts.filter(p=>p.path==='/api/command').length===2,'Enter submits once');
  await route('settings');
  await js('window.savedForm = document.querySelector("#bot-form-0")');
  await click('#bot-form-0 [name=enabled]');
  await click('#bot-form-0 [type=submit]');
  await until(() => posts.some(p => p.path === '/api/settings/bot'), 'connection save');
  assert.equal(posts.find(p => p.path === '/api/settings/bot').body.enabled, false);
  // 保存之后整页会重画一次（POST 收到回包才 render，比上面那条 until 晚）。不等它落地，
  // 紧接着加的那条草稿会被这次重画抹掉，测试就变成谁先跑完谁赢。
  await until(() => js('return !window.savedForm.isConnected'), 'settings re-rendered after save');
  // 草稿那一条（还没落过配置）的「删掉这条」不能当成第 0 条：那会删掉配置里第一条真连接。
  // 用脚本点：这一步测的是处理逻辑，底部那条消息条会挡住真实点击的落点。
  await js('document.querySelector("[data-add-bot]").click()');
  assert.equal(await js('return !!document.querySelector(\'[data-index=""]\')'), true, 'draft bot form appears');
  const saves = posts.filter(p => p.path === '/api/settings/bot').length;
  await js('document.querySelector(\'[data-index=""] [data-drop-bot]\').click()');
  await sleep(200);
  assert.equal(await js('return !!document.querySelector(\'[data-index=""]\')'), false, 'draft leaves the page');
  assert.equal(posts.filter(p => p.path === '/api/settings/bot').length, saves, 'removing a draft must not write to the config');
  await route('logs'); await until(() => streams.size === 1, 'SSE connected');
  await until(() => js('return document.querySelectorAll("#log-box .log-line").length > 0'), 'snapshot');
  // The latest row must stay visible when the viewport/keyboard changes its height,
  // even when no new log arrives to trigger another pin.
  await js('document.querySelector("#log-box").style.flex="0 0 240px"; document.querySelector("#log-box").style.height="240px"');
  await sleep(200);
  assert(await js('const b=document.querySelector("#log-box"); return b.scrollHeight-b.scrollTop-b.clientHeight<3'), 'resize keeps the newest line visible');
  await js('document.querySelector("#log-box").style.flex=""; document.querySelector("#log-box").style.height=""');
  await sleep(200);
  await js(`window.longTasks=[]; new PerformanceObserver(list=>window.longTasks.push(...list.getEntries().map(e=>e.duration))).observe({type:'longtask'});
    window.logMutations=0; new MutationObserver(()=>window.logMutations++).observe(document.querySelector('#log-box'),{childList:true});`);
  emit(2400); await sleep(600);
  assert.equal(await js('return document.querySelector("#log-box").children.length'), 80);
  assert.equal(await js('return !!window.injected'), false);
  assert.equal(await js('return document.querySelector("#log-follow").getAttribute("aria-pressed")'), 'true');
  assert.equal(await js('return document.querySelector("#log-box .log-line:last-child .log-text").textContent'), history.at(-1).text, 'burst shows the newest row');
  await click('#log-follow');
  await js('document.querySelector("#log-box").dispatchEvent(new Event("scroll"))');
  assert.equal(await js('return document.querySelector("#log-follow").getAttribute("aria-pressed")'), 'false', 'late programmatic scroll must not undo an explicit pause');
  const paused = await js('return document.querySelector("#log-box").innerHTML');
  emit(30); await sleep(180);
  assert.equal(await js('return document.querySelector("#log-box").innerHTML'), paused, 'paused DOM must stay stable');
  await click('#log-jump');
  await click('[data-level=WARN]');
  assert.equal(await js('return [...document.querySelectorAll("#log-box .log-line")].every(n=>n.classList.contains("log-warn"))'), true);
  const sparse = [{ ...line('rare warning at the start of a burst'), level: 'WARN' },
    ...Array.from({length: 300}, () => ({ ...line('routine information'), level: 'INFO' }))];
  history = history.concat(sparse).slice(-2000);
  for (const response of streams) response.write(`event: batch\ndata: ${JSON.stringify({lines:sparse})}\n\n`);
  await sleep(350);
  assert.equal(await js('return document.querySelector("#log-box .log-line:last-child .log-text").textContent'), sparse[0].text, 'filter before queue truncation preserves latest matching row');
  await js(`window.makeURL=URL.createObjectURL; window.anchorClick=HTMLAnchorElement.prototype.click;
    URL.createObjectURL=blob=>{window.exportedLog=blob;return window.makeURL(blob)};
    HTMLAnchorElement.prototype.click=function(){window.exportedName=this.download};
    document.querySelector('#log-export').click();
    URL.createObjectURL=window.makeURL; HTMLAnchorElement.prototype.click=window.anchorClick;`);
  const exported = await cmd('POST', '/execute/async', { script: 'window.exportedLog.text().then(arguments[arguments.length-1])', args: [] });
  assert(exported.includes(sparse[0].text) && !exported.includes('[INFO]'), 'export includes filtered buffer, not only the DOM');
  assert.match(await js('return window.exportedName'), /^zhiwei-logs-.*\.txt$/);
  await js('document.querySelector("#log-box").scrollTop=0');
  await until(() => js('return document.querySelector("#log-follow").getAttribute("aria-pressed")==="false"'), 'reading older rows pauses');
  await js('const b=document.querySelector("#log-box"); b.scrollTop=b.scrollHeight');
  await until(() => js('return document.querySelector("#log-follow").getAttribute("aria-pressed")==="true"'), 'scrolling to bottom resumes');
  // Changing filters must preserve toolbar nodes, focus and the SSE subscription.
  await js('window.searchNode=document.querySelector("#log-search")');
  await click('[data-level=""]');
  assert.equal(await js('return window.searchNode===document.querySelector("#log-search")'), true);
  await route('plugins'); await until(() => streams.size === 0, 'SSE closes on navigation');
  await route('logs'); await until(() => streams.size === 1, 'SSE reconnects');
  await click('#log-follow');
  const beforeBackground = await js('const b=document.querySelector("#log-box"); return {html:b.innerHTML,top:b.scrollTop}');
  const original = await cmd('GET', '/window');
  const tab = await cmd('POST', '/window/new', { type: 'tab' });
  await cmd('POST', '/window', { handle: tab.handle });
  await until(() => streams.size === 0, 'hidden tab disconnects');
  emit(4);
  await cmd('DELETE', '/window'); await cmd('POST', '/window', { handle: original });
  await until(() => streams.size === 1, 'visible tab resumes');
  await sleep(150);
  assert.deepEqual(await js('const b=document.querySelector("#log-box"); return {html:b.innerHTML,top:b.scrollTop}'), beforeBackground, 'paused reading survives hidden tab and new snapshot');
  await click('#log-jump');
  assert.equal(await js('return document.querySelector("#log-box .log-line:last-child .log-text").textContent'), history.at(-1).text);
  const metrics = await js('return {longTasks:window.longTasks, logMutations:window.logMutations}');
  // 这一批改动的中心就是「2400 行突发不长任务、DOM 按 100ms 合批」。
  // 数字采到了就要判定，否则把 DOM 上限调回 500、或改回逐条写 DOM，测试照样全绿。
  assert(Math.max(0, ...metrics.longTasks) < 50, `no long task over 50ms (got ${JSON.stringify(metrics.longTasks)})`);
  assert(metrics.logMutations <= 30, `log DOM stays batched (got ${metrics.logMutations} mutations)`);
  // A non-SSE response closes EventSource permanently; the app must reopen it.
  rejectStreams = true;
  for (const response of streams) response.end();
  await until(() => js('return document.querySelector("#log-status").textContent.includes("秒后重试")'), 'closed SSE backoff');
  rejectStreams = false;
  await until(() => streams.size === 1, 'closed SSE recovers');
  await route('settings');
  await click('[data-density-choice=comfortable]');
  assert.equal(await js('return getComputedStyle(document.documentElement).getPropertyValue("--md-type-body-medium-size").trim()'), '15px');
  await cmd('POST', '/refresh', {});
  await until(() => js('return document.documentElement.dataset.density==="comfortable" && !!document.querySelector("[data-density-choice]")'), 'density persists across reload');
  await click('[data-density-choice=compact]');
  assert.equal(await js('return getComputedStyle(document.documentElement).getPropertyValue("--md-type-body-medium-size").trim()'), '13px');
  history = Array.from({length:20},(_,i)=>{
    const [level,text] = [['INFO','已连接实现端，开始接收消息'],['INFO','配置已保存，下一条消息生效'],['WARN','请求暂未回应，等待重试'],['INFO','已完成本轮消息处理'],['ERRO','图片下载失败，请稍后重试']][i%5];
    return {...line(text),level};
  });
  const screenshots = ['overview','plugins','plugins/oai','plugins/console','ambient','logs','command','settings'];
  for (const [label, width, height] of [['compact',390,844],['narrow',320,740],['medium',800,1000],['expanded',1400,900]]) {
    await viewport(width,height);
    for (const theme of ['light','dark']) {
      await media([{ name:'prefers-color-scheme', value:theme }]);
      for (const page of screenshots) {
        await route(page); await js('window.scrollTo(0,0)'); await sleep(150);
        assert(await js('return document.documentElement.scrollWidth <= innerWidth'), `overflow: ${label}/${theme}/${page}`);
        if (page === 'logs') assert(await js('const b=document.querySelector("#log-box"); const t=document.querySelector(".log-tools"); return b.scrollHeight-b.scrollTop-b.clientHeight<3 && t.getBoundingClientRect().bottom <= b.getBoundingClientRect().top'), `latest visible and toolbar does not cover logs: ${label}/${theme}`);
        await shot(`${label}-${theme}-${page.replace('/','-')}`);
      }
    }
  }
  // 「减少动态效果」要测的是 app.js 自己那两处降级，不是 CSS：换页动画整条写在
  // @media (prefers-reduced-motion: no-preference) 里，只看 computed 的话，
  // 把 JS 侧的守卫删掉也永远是 none。两条各配一个正对照。
  const pressRefresh = () => js('document.querySelector("#refresh").dispatchEvent(new PointerEvent("pointerdown",{bubbles:true,button:0,isPrimary:true}))');
  const enterAnimates = () => js('return document.querySelector("#view").dataset.enter === ""');
  await media([{ name:'prefers-reduced-motion', value:'no-preference' }]);
  await route('logs'); await route('overview');
  assert.equal(await enterAnimates(), true, 'motion allowed: page enter animation is set');
  await pressRefresh();
  assert((await js('return document.querySelectorAll(".ripple").length')) >= 1, 'motion allowed: ripple appears');
  await media([{ name:'prefers-reduced-motion', value:'reduce' }]);
  await js('document.querySelector("#view").removeAttribute("data-enter")');
  // 上一轮留下的涟漪在 reduce 下动画被关掉，等不到 animationend 自行消失，先清干净。
  await js('for (const node of document.querySelectorAll(".ripple")) node.remove()');
  await route('logs'); await route('overview');
  assert.equal(await enterAnimates(), false, 'reduced motion: no page enter animation');
  assert.equal(await js('return getComputedStyle(document.querySelector("#view")).animationName'), 'none');
  await pressRefresh();
  assert.equal(await js('return document.querySelectorAll(".ripple").length'), 0, 'reduced motion: no ripple');
  await route('plugins');
  assert(await js('return [...document.querySelectorAll("button:not([disabled])")].filter(e=>e.getClientRects().length).every(e=>e.getBoundingClientRect().height >= 48)'), '48px button touch targets');
  const errors = (await cmd('POST', '/log', { type:'browser' })).filter(e => e.level === 'SEVERE' && !e.message.includes('404') && !e.message.includes('503'));
  assert.deepEqual(errors, [], 'no JavaScript errors');
  if (out) fs.writeFileSync(path.join(out, 'metrics.json'), JSON.stringify(metrics, null, 2));
  console.log('Console browser checks passed: navigation, switches, stale responses, dialogs, forms, bounded logs, sparse filters, export, resize pinning, pause, visibility, reconnect, density persistence, themes, 320–1400px and reduced motion.');
  console.log(JSON.stringify(metrics));
})().catch(error => { console.error(error); process.exitCode=1; }).finally(async () => {
  if (session) await cmd('DELETE','').catch(()=>{});
  driver?.kill(); for (const response of streams) response.end();
  server.closeAllConnections(); server.close();
});
