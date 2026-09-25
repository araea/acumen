// node tests/console.cjs — 真实 Chromium + 隔离的 HTTP 夹具，不碰正在运行的机器人。
// ACUMEN_CONSOLE_SHOTS=<目录> 导出截图与性能数据；ACUMEN_AXE_CORE=<axe.min.js> 追加 axe 扫描。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const { spawn, execFileSync } = require('node:child_process');

const root = path.resolve(__dirname, '..');
const fixturePath = path.join(require('node:os').tmpdir(), `acumen-plugins-${process.pid}.json`);
execFileSync('cargo', ['test', '--locked', 'dump_console_fixture', '--', '--ignored', '--test-threads=1'], {
  cwd: root, env: {...process.env, ACUMEN_CONSOLE_FIXTURE: fixturePath}, stdio: 'pipe'
});
const realPlugins = JSON.parse(fs.readFileSync(fixturePath, 'utf8'));
fs.unlinkSync(fixturePath);
let realMode = false;

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const out = process.env.ACUMEN_CONSOLE_SHOTS;
if (out) fs.mkdirSync(out, { recursive: true });

// —— 夹具数据：形状与 src/plugins/console/api.rs 的回包一致 ——
const sections = [{ code: 'core', name: '核心' }, { code: 'chat', name: '对话' }, { code: 'tools', name: '工具' }];
const plugins = [
  ['ctl', '控制', 'core', '查看与修改插件开关和配置'],
  ['logger', '日志', 'core', '记录运行日志'],
  ['console', '控制台', 'core', '本机网页控制台'],
  ['oai', '智能对话', 'chat', '与模型对话，管理房间与预设'],
  ['ambient', '搭话', 'chat', '参与群聊，记住熟悉的人'],
  ['stats', '统计', 'tools', '消息统计与排行图表'],
  ['webshot', '网页截图', 'tools', '把群里的链接截成图'],
].map(([name, display, section, summary], index) => ({
  name, display, section, summary, on: index !== 5, pending: name === 'ambient',
  effect: '下一条消息生效', commands: name === 'oai' ? [{ cmd: '/oai 房间', note: '列出房间' }, { cmd: '/oai 新建 <名字>', note: '新建一个房间' }] : [],
  config: { enabled: index !== 5, model: '示例模型', retries: 2, temperature: 0.7, stream: true,
    groups: [175131947, 818965288], prompt: '你是一个耐心的助手。\n先听懂，再回答。',
    limits: { per_hour: 30, cooldown: 12, peak: { mode: 'sleep', enabled: false } } },
  defaults: {}, diff: name === 'oai' ? ['model: "默认模型" → "示例模型"'] : [],
}));
const settings = {
  bots: [{ enabled: true, protocol: 'satori', url: 'http://127.0.0.1:3001', has_token: true }],
  command_prefix: ['/'], browser_path: '',
  global_filter: { enable_blacklist: false, blacklist: [], enable_whitelist: true, whitelist: [175131947] },
};
const ambient = {
  ready: true, persona: '自然参与群聊，先听懂，再开口。', self: '我叫知微。',
  memory: [
    { group: '175131947', people: [
      { id: '1001', name: '白虎', note: '喜欢聊驾校和猫', address: '虎哥', messages: 812, exchanges: 40, last_seen: Date.now() / 1000 - 600 },
      { id: '1002', name: '青龙', note: '', address: '', messages: 12, exchanges: 0, last_seen: Date.now() / 1000 - 86400 * 3 },
    ], notes: [{ text: '周五晚上大家一起打了一局狼人杀', at: Date.now() / 1000 - 7200 }] },
    { group: '818965288', people: [], notes: [] },
  ],
  stickers: Array.from({ length: 75 }, (_, i) => ({ id: i + 1, label: i % 7 ? `表情 ${i + 1}` : '', from: '1001', group: '175131947', uses: i, added_at: Date.now() / 1000 - i * 3600, image: i % 11 !== 0 })),
  config: {},
};
const token = 'fixture token';
let posts = [], streams = new Set(), sequence = 0, detailDelay = false, rejectStreams = false;
const line = text => ({ at: '12:30:00', level: ['INFO', 'WARN', 'ERRO', 'DEBG'][sequence++ % 4], target: 'Plugin/Console', text });
let history = Array.from({ length: 80 }, (_, i) => line(`运行记录 ${i} · 已完成处理`));
function emit(count) {
  const lines = Array.from({ length: count }, () => line(`压力样本 ${sequence} <img src=x onerror="window.injected=true">`));
  history = history.concat(lines).slice(-2000);
  const batches = [];
  for (let i = 0; i < lines.length; i += 128) batches.push(`event: batch\ndata: ${JSON.stringify({ lines: lines.slice(i, i + 128) })}\n\n`);
  for (const response of streams) response.write(batches.join(''));
}
const png = fs.readFileSync(path.join(root, 'res/console/icon-192.png'));

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const reply = (data, status = 200) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(data)); };
  if (url.pathname.startsWith('/api') && req.headers['x-acumen-token'] !== token && url.searchParams.get('t') !== token) {
    return reply({ error: '口令不对' }, 401);
  }
  if (req.method === 'POST') {
    let data = ''; for await (const chunk of req) data += chunk;
    const body = JSON.parse(data); posts.push({ path: url.pathname, body });
    const match = url.pathname.match(/^\/api\/plugins\/([^/]+)\/enabled$/);
    if (match) { const plugin = plugins.find(p => p.name === match[1]); plugin.on = body.on; plugin.pending = body.on && plugin.name === 'stats'; }
    if (url.pathname.endsWith('/config') && body.path === 'retries' && body.value > 10) { await sleep(40); return reply({ error: 'retries 不能大于 10' }, 400); }
    await sleep(60);
    return reply({ message: `已保存 · ${body.input || body.path || ''}` });
  }
  if (url.pathname === '/api/overview') return reply({
    app: { name: '知微', version: '0.1.0', started: '2026-09-16 08:30:00', uptime: 8426 },
    bots: [{ name: '知微', adapter: 'satori', platform: 'QQ', id: '10001' }],
    plugins: { on: 22, total: 23, pending: 1 }, messages: { today: 3803, people: 711, week: 84737 },
    console: { address: 'http://127.0.0.1:7801/' },
  });
  if (url.pathname === '/api/plugins') return reply({ plugins: (realMode ? realPlugins : plugins).map(({ config, defaults, diff, commands, ...rest }) => ({ ...rest, commands: commands.length })), sections });
  if (url.pathname.startsWith('/api/plugins/')) {
    const plugin = (realMode ? realPlugins : plugins).find(p => url.pathname === `/api/plugins/${p.name}`);
    if (detailDelay && plugin?.name === 'oai') await sleep(400);
    return plugin ? reply(plugin) : reply({ error: '这里没有它' }, 404);
  }
  if (url.pathname === '/api/settings') return reply(settings);
  if (url.pathname === '/api/ambient') return reply(ambient);
  if (url.pathname.startsWith('/api/ambient/sticker/')) { res.writeHead(200, { 'Content-Type': 'image/png' }); return res.end(png); }
  if (url.pathname === '/api/logs') return reply({ lines: history.slice(-Number(url.searchParams.get('limit') || 2000)) });
  if (url.pathname === '/api/logs/stream') {
    if (rejectStreams) return reply({ error: 'restarting' }, 503);
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
    res.write(`event: snapshot\ndata: ${JSON.stringify({ lines: history })}\n\n`);
    streams.add(res); res.on('close', () => streams.delete(res)); return;
  }
  const assets = {
    '/': ['index.html', 'text/html'], '/app.js': ['app.js', 'text/javascript'], '/app.css': ['app.css', 'text/css'],
    '/icon.svg': ['icon.svg', 'image/svg+xml'], '/icon-monochrome.svg': ['icon-monochrome.svg', 'image/svg+xml'],
    '/manifest.webmanifest': ['manifest.webmanifest', 'application/manifest+json'],
  };
  if (!assets[url.pathname]) { res.writeHead(404); return res.end(); }
  const [file, type] = assets[url.pathname];
  res.writeHead(200, { 'Content-Type': type });
  // 与 assets.rs 的 stylesheet() 同一顺序：令牌层 → 组件层。
  if (file === 'app.css') res.write(fs.readFileSync(path.join(root, 'res/console/tokens.css')) + '\n');
  res.end(fs.readFileSync(path.join(root, 'res/console', file)));
});

// —— WebDriver ——
const axeSource = process.env.ACUMEN_AXE_CORE ? fs.readFileSync(process.env.ACUMEN_AXE_CORE, 'utf8') : null;
const contrastProbe = execFileSync('python3', ['-c', 'import runpy; print(runpy.run_path("scripts/audit-contrast.py")["PROBE"])'], { cwd: root, encoding: 'utf8' });
const driverPort = Number(process.env.CHROMEDRIVER_PORT || 9529);
let session, driver;
async function call(method, route, data) {
  const response = await fetch(`http://127.0.0.1:${driverPort}${route}`, { method, headers: { 'Content-Type': 'application/json' },
    body: data === undefined ? undefined : JSON.stringify(data), signal: AbortSignal.timeout(60000) });
  const result = (await response.json()).value;
  if (result?.error) throw new Error(`${result.error}: ${result.message}`);
  return result;
}
const cmd = (method, route, data) => call(method, `/session/${session}${route}`, data);
const js = (script, ...args) => cmd('POST', '/execute/sync', { script, args });
const cdp = (name, params) => cmd('POST', '/goog/cdp/execute', { cmd: name, params });
async function until(predicate, name, tries = 120) {
  for (let i = 0; i < tries; i++) { if (await predicate()) return; await sleep(50); }
  throw new Error(`等不到：${name}`);
}
async function element(selector) {
  const node = await cmd('POST', '/element', { using: 'css selector', value: selector });
  return node['element-6066-11e4-a52e-4f735466cecf'];
}
async function click(selector) {
  await js('document.querySelector(arguments[0]).scrollIntoView({block:"center"})', selector);
  await sleep(60);
  await cmd('POST', `/element/${await element(selector)}/click`, {});
}
async function type(selector, text) {
  await cmd('POST', `/element/${await element(selector)}/value`, { text });
}
const key = value => cmd('POST', '/actions', { actions: [{ type: 'key', id: 'keyboard', actions: [{ type: 'keyDown', value }, { type: 'keyUp', value }] }] });
const KEY = { tab: '\uE004', enter: '\uE007', escape: '\uE00C', right: '\uE014', end: '\uE010' };
const ready = () => js('return !document.querySelector("#view[aria-busy], #plugin-detail[aria-busy]") && !!document.querySelector(".page-title") && !document.querySelector(".skeleton")');
async function route(name) {
  await js('location.hash = arguments[0]', '#/' + name);
  await until(ready, `打开 ${name}`);
  await sleep(120);
}
const media = features => cdp('Emulation.setEmulatedMedia', { features });
async function viewport(width, height) {
  await cdp('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
  await sleep(200);
}
async function shot(name) {
  if (!out) return;
  fs.writeFileSync(path.join(out, `${name}.png`), Buffer.from(await cmd('GET', '/screenshot'), 'base64'));
}
const posted = where => posts.filter(p => p.path === where);

(async () => {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  driver = spawn('chromedriver', [`--port=${driverPort}`, '--log-level=SEVERE'], { stdio: 'ignore' });
  await until(async () => { try { await call('GET', '/status'); return true; } catch { return false; } }, 'chromedriver');
  session = (await call('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'chrome',
    'goog:chromeOptions': { args: ['--headless=new', '--no-sandbox', '--disable-gpu', '--lang=zh-CN'] },
    'goog:loggingPrefs': { browser: 'ALL' } } } })).sessionId;
  const base = `http://127.0.0.1:${server.address().port}/`;

  // —— 解锁：错误口令进解锁页，正确口令进总览，地址栏里的口令被抹掉 ——
  await viewport(390, 844);
  await cmd('POST', '/url', { url: `${base}?t=wrong` });
  await until(() => js('return !!document.querySelector("#lock-form")'), '解锁页');
  assert.equal(await js('return document.body.dataset.state'), 'locked');
  assert.equal(await js('return getComputedStyle(document.querySelector("#nav")).display'), 'none', '锁定时不显示导航');
  await type('#lock-input', token);
  await key(KEY.enter);
  await until(() => js('return !!document.querySelector(".hero")'), '解锁后进入总览');
  assert.equal(await js('return location.search'), '', '口令不留在地址栏');
  await cmd('POST', '/url', { url: `${base}?t=${encodeURIComponent(token)}#/overview` });
  await until(() => js('return !!document.querySelector(".hero")'), '带口令地址直接进入');
  assert.equal(await js('return location.search'), '');

  // —— 导航：五格都是真链接，换页后焦点落在新页的 h1 上 ——
  for (const [name, title] of [['plugins', '插件'], ['ambient', '搭话'], ['logs', '日志'], ['settings', '设置'], ['overview', '总览']]) {
    await click(`[data-nav="${name}"]`);
    await until(ready, name);
    assert.equal(await js('return location.hash'), '#/' + name);
    assert.equal(await js('return document.querySelector("[aria-current=page]").dataset.nav'), name);
    assert.equal(await js('return document.activeElement.classList.contains("page-title") && document.activeElement.textContent'), title);
    assert.equal(await js('return document.title'), `${title} · 知微`);
  }
  assert.equal(await js('return document.querySelectorAll("#nav a[href]").length'), 5);

  // —— 窄屏插件：搜索只重画列表；详情独立成页，带返回 ——
  await route('plugins');
  await js('window.searchNode = document.querySelector("#plugin-search")');
  await type('#plugin-search', '对话');
  await until(() => js('return document.querySelectorAll("#plugin-list [data-plugin]").length === 1'), '搜索过滤');
  assert.equal(await js('return document.querySelector("#plugin-count").textContent'), '显示 1 / 7 个');
  assert.equal(await js('return window.searchNode === document.activeElement'), true, '搜索框保持焦点');
  await js('const s=document.querySelector("#plugin-search"); s.value=""; s.dispatchEvent(new Event("input",{bubbles:true}))');
  await click('[data-section="tools"]');
  assert.equal(await js('return document.querySelectorAll("#plugin-list [data-plugin]").length'), 2, '按分类筛选');
  assert.equal(await js('return document.querySelector("[data-section=tools]").getAttribute("aria-pressed")'), 'true');
  await click('[data-section=""]');
  await click('[data-plugin="oai"] .item-link');
  await until(() => js('return location.hash === "#/plugins/oai" && document.querySelector(".page-title")?.textContent === "智能对话"'), '窄屏详情页');
  assert.equal(await js('return !!document.querySelector(".page-head .back[href=\\"#/plugins\\"]")'), true, '详情页有返回');
  assert.equal(await js('return document.title'), '智能对话 · 知微');

  // —— 宽屏：列表-详情并排，行内开关与行链接互不干扰，慢回包不能盖过后点的 ——
  await viewport(1400, 900);
  await route('plugins');
  await click('[data-plugin="logger"] .switch');
  await until(() => posted('/api/plugins/logger/enabled').length === 1, '开关 POST');
  assert.equal(await js('return location.hash'), '#/plugins', '点开关不换页');
  await until(() => js('return document.querySelector("[data-plugin=logger] .switch").getAttribute("aria-checked") === "false"'), '开关以回执为准');
  await js('window.listNode = document.querySelector("#plugin-list")');
  detailDelay = true;
  await js('document.querySelector("[data-plugin=oai] .item-link").click(); setTimeout(()=>document.querySelector("[data-plugin=console] .item-link").click(), 30)');
  await sleep(700);
  detailDelay = false;
  assert.equal(await js('return location.hash'), '#/plugins/console');
  assert.equal(await js('return document.querySelector("#detail-title .tag").textContent'), 'console', '先发后到的回包不覆盖');
  assert.equal(await js('return window.listNode === document.querySelector("#plugin-list")'), true, '列表节点保留');
  assert.equal(await js('return document.querySelector("[data-plugin=console] .item-link").getAttribute("aria-current")'), 'page');

  // —— 配置：回车或离开即保存；数字校验在行内提示；布尔项立即提交；差异刷新 ——
  await click('[data-plugin="oai"] .item-link');
  await until(() => js('return document.querySelector("#detail-title .tag")?.textContent === "oai"'), '打开 oai');
  const retries = '[data-path="retries"]';
  await js(`document.querySelector('${retries}').value='abc'`);
  await js(`document.querySelector('${retries}').focus()`);
  await key(KEY.enter);
  await until(() => js(`return document.querySelector('${retries}').getAttribute('aria-invalid') === 'true'`), '数字校验');
  assert.equal(await js(`return document.querySelector('#' + document.querySelector('${retries}').id + '-error').getAttribute('role')`), 'alert');
  assert.equal(posts.filter(p => p.path.endsWith('/config')).length, 0, '校验不过不提交');
  await js(`document.querySelector('${retries}').value='99'`);
  await key(KEY.enter);
  await until(() => js(`return document.querySelector('#' + document.querySelector('${retries}').id + '-error')?.textContent.includes('不能大于')`), '后端错误落到这一格');
  await js(`const r=document.querySelector('${retries}'); r.value='5'`);
  await key(KEY.enter);
  await until(() => posts.some(p => p.path === '/api/plugins/oai/config' && p.body.value === 5), '合法值提交');
  await until(() => js(`return !document.querySelector('${retries}').hasAttribute('aria-invalid')`), '错误清除');
  await key(KEY.tab);
  await sleep(200);
  assert.equal(posts.filter(p => p.path === '/api/plugins/oai/config' && p.body.path === 'retries').length, 2, '回车已存，离开不重复提交');
  await js(`const g=document.querySelector('[data-path="groups"]'); g.value='[1, 2, 3]'; g.dispatchEvent(new Event('change',{bubbles:true}))`);
  await until(() => posts.some(p => p.body.path === 'groups'), '列表提交');
  assert.deepEqual(posts.find(p => p.body.path === 'groups').body.value, [1, 2, 3]);
  await click('[data-path="limits.peak.enabled"]');
  await until(() => posts.some(p => p.body.path === 'limits.peak.enabled' && p.body.value === true), '嵌套布尔项');
  assert.equal(await js('return !!document.querySelector("fieldset.config-group legend")'), true, '表展开为 fieldset');
  assert.equal(await js('return !document.querySelector("[data-path=enabled]")'), true, '顶层 enabled 只由启用开关管');

  // —— 确认对话框：初始焦点在取消、Tab 不出框、Esc 取消并回到触发点 ——
  await click('[data-reset]');
  await until(() => js('return document.querySelector("dialog").open'), '对话框');
  assert.equal(await js('return document.activeElement.value'), 'cancel');
  await key(KEY.tab);
  assert.equal(await js('return document.querySelector("dialog").contains(document.activeElement)'), true);
  await key(KEY.escape);
  await until(() => js('return !document.querySelector("dialog").open'), '关闭对话框');
  assert.equal(await js('return document.activeElement.matches("[data-reset]")'), true, '焦点回到触发点');
  assert.equal(posted('/api/plugins/oai/reset').length, 0);
  await click('[data-reset]');
  await until(() => js('return document.querySelector("dialog").open'), '再次打开');
  await click('dialog [value=confirm]');
  await until(() => posted('/api/plugins/oai/reset').length === 1, '确认后重置');

  // —— 搭话：页签按 APG 走方向键；编辑出现未保存标记，保存后清除；表情包分页 ——
  await route('ambient');
  await js('document.querySelector("#tab-persona").focus()');
  await key(KEY.right);
  assert.equal(await js('return document.activeElement.id'), 'tab-memory');
  assert.deepEqual(await js('return [document.querySelector("#panel-memory").hidden, document.querySelector("#panel-persona").hidden]'), [false, true], '自动激活：面板跟着切换');
  await key(KEY.end);
  assert.equal(await js('return document.activeElement.id + "|" + document.querySelector("#tab-stickers").getAttribute("aria-selected")'), 'tab-stickers|true');
  assert.equal(await js('return document.querySelectorAll("#sticker-grid > li").length'), 60);
  await click('[data-more-stickers]');
  assert.equal(await js('return document.querySelectorAll("#sticker-grid > li").length'), 75);
  assert.equal(await js('return document.activeElement === document.querySelectorAll("#sticker-grid > li")[60]'), true, '焦点落到新出现的第一张');
  await click('#tab-persona');
  assert.equal(await js('return document.querySelector("[data-save-source=persona]").disabled'), true);
  await type('#source-persona', '补一句。');
  assert.equal(await js('return document.querySelector("#source-persona-state").hasAttribute("data-dirty")'), true);
  await click('[data-save-source=persona]');
  await until(() => posted('/api/ambient/source').length === 1, '保存人格');
  assert.equal(posted('/api/ambient/source')[0].body.text, '自然参与群聊，先听懂，再开口。补一句。');
  await until(() => js('return !document.querySelector("#source-persona-state").hasAttribute("data-dirty")'), '保存后清除标记');

  // —— 设置：命令、连接草稿、全局校验 ——
  await route('settings');
  await type('#command-input', 'list');
  await key(KEY.enter);
  await until(() => js('return document.querySelector("#command-output").textContent.includes("已保存")'), '命令回执');
  assert.equal(posted('/api/command').length, 1, '回车只提交一次');
  await click('[data-run="show ambient"]');
  await until(() => posted('/api/command').length === 2, '快捷命令');
  await js('window.botForm = document.querySelector("[data-bot=\\"0\\"]")');
  await type('#g-browser', '/unsaved/browser');
  await click('[data-bot="0"] [name=enabled]');
  await click('[data-bot="0"] [type=submit]');
  await until(() => posted('/api/settings/bot').length === 1, '保存连接');
  assert.equal(posted('/api/settings/bot')[0].body.enabled, false);
  assert.equal('access_token' in posted('/api/settings/bot')[0].body, false, '令牌留空即不动');
  await until(() => js('return !window.botForm.isConnected'), '保存后重画');
  assert.equal(await js('return document.querySelector("#g-browser").value'), '/unsaved/browser', '保存连接保留全局草稿');
  await viewport(800, 900);
  assert.equal(await js('return document.querySelector("#g-browser").value'), '/unsaved/browser', '断点变化保留草稿');
  await viewport(1400, 900);
  await js('document.querySelector("#g-browser").value = document.querySelector("#g-browser").defaultValue');

  await click('[data-add-bot]');
  assert.equal(await js('return document.activeElement.id'), 'bot-new-url', '新草稿聚焦地址');
  await click('[data-bot=""] [data-drop-bot]');
  assert.equal(await js('return !document.querySelector("[data-bot=\\"\\"]")'), true, '草稿离开页面');
  assert.equal(posted('/api/settings/bot').length, 1, '放弃草稿不写配置');
  await js('document.querySelector("#g-white").value = "175131947, 群二"');
  await click('#global-form [type=submit]');
  await until(() => js('return document.querySelector("#g-white").getAttribute("aria-invalid") === "true"'), '群号校验');
  assert.equal(posted('/api/settings/global').length, 0);
  await js('document.querySelector("#g-white").value = "175131947"');
  await click('#global-form [type=submit]');
  await until(() => posted('/api/settings/global').length === 1, '保存全局');
  assert.deepEqual(posted('/api/settings/global')[0].body.global_filter.whitelist, [175131947]);
  await until(() => js('return !document.querySelector("#global-form[aria-busy]")'), '全局设置完成保存');

  // —— 日志：突发有界、可暂停、筛选先于截断、导出、后台断流、断线退避 ——
  await route('logs');
  await until(() => streams.size === 1, 'SSE 连接');
  await until(() => js('return document.querySelectorAll("#log-box .log-row").length > 0'), '快照');
  await js(`window.longTasks=[]; window.perf=new PerformanceObserver(l=>window.longTasks.push(...l.getEntries().map(e=>e.duration))); window.perf.observe({type:'longtask'});
    window.mutations=0; new MutationObserver(()=>window.mutations++).observe(document.querySelector('#log-box'),{childList:true});`);
  emit(2400); await sleep(800);
  await js('window.longTasks.push(...window.perf.takeRecords().map(e=>e.duration)); window.perf.disconnect()');
  const metrics = await js('return {longTasks: window.longTasks, mutations: window.mutations}');
  assert.equal(await js('return document.querySelector("#log-box").children.length'), 100, 'DOM 窗口 100 行');
  assert.equal(await js('return !!window.injected'), false, '日志正文转义');
  assert.equal(await js('return document.querySelector("#log-box .log-row:last-child .log-text").textContent'), history.at(-1).text, '突发后显示最新一行');
  assert(Math.max(0, ...metrics.longTasks) < 50, `突发没有超过 50ms 的长任务：${JSON.stringify(metrics.longTasks)}`);
  assert(metrics.mutations <= 30, `DOM 合批：${metrics.mutations}`);
  await click('#log-follow');
  assert.equal(await js('return document.querySelector("#log-follow").getAttribute("aria-pressed")'), 'false');
  await js('document.querySelector("#log-box").dispatchEvent(new Event("scroll"))');
  assert.equal(await js('return document.querySelector("#log-follow").getAttribute("aria-pressed")'), 'false', '程序滚动不撤销手动暂停');
  const paused = await js('return document.querySelector("#log-box").innerHTML');
  emit(30); await sleep(250);
  assert.equal(await js('return document.querySelector("#log-box").innerHTML'), paused, '暂停时 DOM 不动');
  assert.match(await js('return document.querySelector("#log-jump span").textContent'), /30 条新记录/);
  await click('#log-jump');
  assert.equal(await js('return document.querySelector("#log-jump").hidden'), true);
  await click('.segment:has(input[value=WARN])');
  assert.equal(await js('return [...document.querySelectorAll("#log-box .log-row")].every(n => n.dataset.level === "warn")'), true, '按级别筛选');
  const sparse = [{ ...line('罕见的一条警告'), level: 'WARN' }, ...Array.from({ length: 300 }, () => ({ ...line('例行信息'), level: 'INFO' }))];
  history = history.concat(sparse).slice(-2000);
  for (const response of streams) response.write(`event: batch\ndata: ${JSON.stringify({ lines: sparse })}\n\n`);
  await sleep(400);
  assert.equal(await js('return document.querySelector("#log-box .log-row:last-child .log-text").textContent'), '罕见的一条警告', '先筛选后截断');
  await js(`window.makeURL=URL.createObjectURL; window.anchorClick=HTMLAnchorElement.prototype.click;
    URL.createObjectURL=b=>{window.exported=b;return window.makeURL(b)}; HTMLAnchorElement.prototype.click=function(){window.exportedName=this.download};
    document.querySelector('#log-export').click(); URL.createObjectURL=window.makeURL; HTMLAnchorElement.prototype.click=window.anchorClick;`);
  const exported = await cmd('POST', '/execute/async', { script: 'window.exported.text().then(arguments[arguments.length-1])', args: [] });
  assert(exported.includes('罕见的一条警告') && !exported.includes('[INFO]'), '导出的是当前筛选的缓冲');
  assert.match(await js('return window.exportedName'), /^acumen-logs-.*\.txt$/);
  await click('.segment:has(input[value=""])');
  await js('document.querySelector("#log-box").scrollTop = 0');
  await until(() => js('return document.querySelector("#log-follow").getAttribute("aria-pressed") === "false"'), '向上翻阅即暂停');
  await js('const b=document.querySelector("#log-box"); b.scrollTop=b.scrollHeight');
  await until(() => js('return document.querySelector("#log-follow").getAttribute("aria-pressed") === "true"'), '回到底部恢复');
  await route('plugins'); await until(() => streams.size === 0, '离开日志页断流');
  await route('logs'); await until(() => streams.size === 1, '回来重连');
  const original = await cmd('GET', '/window');
  const tab = await cmd('POST', '/window/new', { type: 'tab' });
  await cmd('POST', '/window', { handle: tab.handle });
  await until(() => streams.size === 0, '后台断流');
  await cmd('DELETE', '/window'); await cmd('POST', '/window', { handle: original });
  await until(() => streams.size === 1, '前台恢复');
  rejectStreams = true;
  for (const response of streams) response.end();
  await until(() => js('return document.querySelector("#log-status").textContent.includes("秒后重试")'), '断线退避');
  rejectStreams = false;
  await until(() => streams.size === 1, '退避后恢复', 200);

  // —— 触控目标：触屏 48，精确指针 40；两者都高于 WCAG 2.5.8 的 24 ——
  await route('plugins');
  const smallest = () => js(`return Math.min(...[...document.querySelectorAll('.btn, .icon-btn, .nav-item')]
    .filter(e => e.getClientRects().length).map(e => Math.min(e.getBoundingClientRect().height, e.getBoundingClientRect().width)))`);
  assert((await smallest()) >= 40, `精确指针下控件不小于 40px：${await smallest()}`);
  await cdp('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  await viewport(390, 844); await route('plugins');
  if (await js('return matchMedia("(pointer: coarse)").matches')) {
    assert((await smallest()) >= 48, `触屏下控件不小于 48px：${await smallest()}`);
  }
  await cdp('Emulation.setTouchEmulationEnabled', { enabled: false });

  // —— 矩阵：四档宽度 × 浅深 × 七个页面；逐元素对比度、边界、名称、目标，可选 axe ——
  history = Array.from({ length: 24 }, (_, i) => {
    const [level, text] = [['INFO', '已连接实现端，开始接收消息'], ['INFO', '配置已保存，下一条消息生效'], ['WARN', '请求暂未回应，等待重试'], ['INFO', '已完成本轮消息处理'], ['ERRO', '图片下载失败，请稍后重试'], ['DEBG', '心跳正常']][i % 6];
    return { ...line(text), level, target: ['Plugin/Ambient', 'Adapter/Satori', 'Plugin/Stats'][i % 3] };
  });
  const accessibility = [];
  const pages = ['overview', 'plugins', 'plugins/oai', 'ambient', 'logs', 'settings'];
  const audit = async label => {
    const contrast = await js(contrastProbe);
    const failures = Object.fromEntries(['text', 'borders', 'nameless', 'tiny', 'hints'].map(k => [k, contrast[k]]));
    if (Object.values(failures).some(items => items.length)) accessibility.push({ page: label, contrast: failures });
    if (axeSource) {
      await js(axeSource);
      const result = await cmd('POST', '/execute/async', { script: `const done=arguments[arguments.length-1];
        axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21a','wcag21aa','wcag22aa']}})
        .then(r=>done({violations:r.violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>n.target)}))})).catch(e=>done({error:String(e)}))`, args: [] });
      if (result.error || result.violations.length) accessibility.push({ page: label, axe: result });
    }
  };
  let snapshots = 0;
  for (const [label, width, height] of [['compact', 390, 844], ['narrow', 320, 720], ['medium', 800, 1000], ['expanded', 1400, 900]]) {
    await viewport(width, height);
    for (const theme of ['light', 'dark']) {
      await media([{ name: 'prefers-color-scheme', value: theme }]);
      for (const page of pages) {
        await route(page); await js('scrollTo(0,0)'); await sleep(250);
        assert(await js('return document.documentElement.scrollWidth <= innerWidth'), `横向溢出：${label}/${theme}/${page}`);
        await shot(`${label}-${theme}-${page.replace('/', '-')}`);
        await audit(`${label}/${theme}/${page}`);
        snapshots++;
      }
    }
  }
  // 搭话的另外两个页签也看一眼（快照只拍默认页签）。
  await viewport(1400, 900); await media([{ name: 'prefers-color-scheme', value: 'light' }]);
  await route('ambient');
  for (const tab of ['memory', 'stickers']) { await click(`#tab-${tab}`); await sleep(300); await shot(`expanded-light-ambient-${tab}`); await audit(`expanded/light/ambient-${tab}`); snapshots++; }
  await click('#tab-persona');
  // 提高对比度两档也要过线（HIG「增强对比度」与浏览器的 prefers-contrast）。
  await viewport(390, 844);
  for (const theme of ['light', 'dark']) {
    await media([{ name: 'prefers-color-scheme', value: theme }, { name: 'prefers-contrast', value: 'more' }]);
    for (const page of pages) { await route(page); await sleep(150); await audit(`contrast-more/${theme}/${page}`); snapshots++; }
    await shot(`contrast-more-${theme}-overview`);
  }
  await media([]);
  if (out) fs.writeFileSync(path.join(out, 'accessibility.json'), JSON.stringify(accessibility, null, 2));
  assert.deepEqual(accessibility, [], '对比度、边界、名称、目标与可选 axe 审计');

  // 所有注册插件使用真实的默认配置与说明，不复用通用假字段。
  realMode = true;
  assert.equal(realPlugins.length, 22);
  for (const width of [320, 1400]) {
    await viewport(width, 900);
    for (const plugin of realPlugins) {
      await route('plugins/' + plugin.name);
      await until(() => js('return document.querySelector("[data-config]")?.dataset.config === arguments[0] || !arguments[1]', plugin.name, Object.keys(plugin.config).some(k=>k!=='enabled')), '真实插件详情');
      assert(await js('return document.documentElement.scrollWidth <= innerWidth'), `真实配置溢出 ${plugin.name}/${width}`);
      const expected = [];
      const walk = (value, prefix='') => Object.entries(value).forEach(([key,item])=> {
        if (!prefix && key==='enabled') return;
        const path = prefix ? prefix + '.' + key : key;
        if (item && typeof item === 'object' && !Array.isArray(item)) walk(item,path); else expected.push(path);
      });
      walk(plugin.config);
      assert.deepEqual((await js('return [...document.querySelectorAll("[data-config] [data-path]")].map(n=>n.dataset.path)')).sort(), expected.sort(), plugin.name + ': all editable fields');
      await audit(`real/${width}/${plugin.name}`);
    }
  }
  assert.deepEqual(accessibility, [], '全部插件真实配置无障碍检查');
  realMode = false;

  // —— WCAG 1.4.12 文字间距：用户覆盖行高字距之后 320px 仍不横向溢出 ——
  await viewport(320, 720);
  for (const page of ['plugins/oai', 'settings', 'logs']) {
    await route(page);
    await js(`const s=document.createElement('style');s.id='spacing';s.textContent='*{line-height:1.5!important;letter-spacing:.12em!important;word-spacing:.16em!important} p{margin-bottom:2em!important}';document.head.append(s)`);
    assert(await js('return document.documentElement.scrollWidth <= innerWidth'), `文字间距覆盖后溢出：${page}`);
    await js('document.querySelector("#spacing").remove()');
  }

  // 200% 文本缩放仍可回流；测试完成后恢复用户默认字号。
  await viewport(390, 844);
  for (const page of ['plugins/oai', 'settings', 'ambient']) {
    await route(page);
    await js('document.documentElement.style.fontSize="200%"');
    assert(await js('return document.documentElement.scrollWidth <= innerWidth'), `200% 字号溢出：${page}`);
    await js('document.documentElement.style.fontSize=""');
  }

  // —— WCAG 2.4.11 焦点不被顶栏 / 底栏完全遮住；焦点环可见 ——
  await viewport(390, 700);
  await route('settings');
  await js('document.querySelector(".page-title").focus()');
  for (let step = 0; step < 30; step++) {
    await key(KEY.tab);
    await sleep(30);
    const ok = await js(`const e=document.activeElement; if(!e || !e.closest('#view')) return true;
      const r=e.getBoundingClientRect(), bar=document.querySelector('.topbar').getBoundingClientRect(), nav=document.querySelector('.nav').getBoundingClientRect();
      const visible = r.bottom > bar.bottom && r.top < nav.top;
      const ring = getComputedStyle(e).outlineStyle !== 'none' || getComputedStyle(e.closest('.item') || e).outlineStyle !== 'none' || e.matches('.input, .textarea, select');
      return visible && ring`);
    assert(ok, `第 ${step + 1} 次 Tab：焦点被遮住或不可见（${await js('return document.activeElement.outerHTML.slice(0,120)')}）`);
  }

  // —— 减少动态效果：入场动画与弹簧位移归零；系统高对比度保留系统色 ——
  await media([{ name: 'prefers-reduced-motion', value: 'reduce' }]);
  await route('logs'); await route('overview');
  assert.equal(await js('return getComputedStyle(document.querySelector(".page")).animationName'), 'none', '减少动态时无入场动画');
  assert.equal(await js('return getComputedStyle(document.querySelector(".btn")).transitionDuration.split(",")[0].trim()'), '0s', '按钮形变归零');
  await media([{ name: 'prefers-reduced-motion', value: 'no-preference' }]);
  await route('logs'); await route('overview');
  assert.notEqual(await js('return getComputedStyle(document.querySelector(".page")).animationName'), 'none', '正常时有入场动画');
  await media([{ name: 'forced-colors', value: 'active' }]);
  assert.notEqual(await js('return getComputedStyle(document.querySelector(".nav-item:not([aria-current])")).forcedColorAdjust'), 'none', '强制色下保留系统色');
  await media([]);

  const errors = (await cmd('POST', '/log', { type: 'browser' })).filter(e => e.level === 'SEVERE' && e.source !== 'network');
  assert.deepEqual(errors, [], '没有脚本错误');
  if (out) fs.writeFileSync(path.join(out, 'metrics.json'), JSON.stringify(metrics, null, 2));
  console.log(`控制台浏览器回归通过：解锁、导航与焦点、列表-详情、行内校验、对话框、页签、表单、日志压力与暂停、后台断流、断线退避、触控目标、${snapshots} 个无障碍快照、文字间距、焦点遮挡、减少动态与强制色。`);
  console.log(JSON.stringify({ ...metrics, snapshots, axe: !!axeSource }));
})().catch(error => { console.error(error); process.exitCode = 1; }).finally(async () => {
  if (session) await cmd('DELETE', '').catch(() => {});
  driver?.kill();
  for (const response of streams) response.end();
  server.closeAllConnections(); server.close();
});
