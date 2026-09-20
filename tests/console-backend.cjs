// node tests/console-backend.cjs — release binary with isolated config/db, foreground stdin.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const { spawn } = require('node:child_process');
const root = path.resolve(__dirname, '..');
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'acumen-console-api-'));
let child, exited, output = '', reader, abort;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(predicate, description) {
  for (let i = 0; i < 200; i++) {
    if (await predicate()) return;
    if (child?.exitCode !== null && child?.exitCode !== undefined) throw new Error('Process exited');
    await sleep(50);
  }
  throw new Error(`Timeout: ${description}`);
}
(async () => {
  const probe = net.createServer();
  await new Promise(resolve => probe.listen(0, '127.0.0.1', resolve));
  const port = probe.address().port;
  await new Promise(resolve => probe.close(resolve));
  const binary = path.join(dir, 'acumen'); fs.copyFileSync(path.join(root, 'target/release/acumen'), binary);
  const registry = fs.readFileSync(path.join(root, 'src/plugins/registry.rs'), 'utf8');
  const names = [...registry.matchAll(/^    ([a-z_]+) \{/gm)].map(m => m[1]);
  // bots = [] 必须写在各插件表之前：不给这个键会落到 Config::default() 里那份
  // 「默认连本机 satori-qq:3001」的模板 bot，这个测试就不再是隔离的（会登录真实现端、
  // 收到真群消息）。口令故意带空格，用来钉住查询串那条载体的百分号编码。
  fs.writeFileSync(path.join(dir, 'config.toml'), 'command_prefix = ["/"]\nbots = []\n' +
    names.map(name => `[${name}]\nenabled = ${['console','ctl','logger'].includes(name)}\n` +
      (name === 'console' ? `port = ${port}\ntoken = "test console only"\nlog_lines = 5000\n` : '') +
      (name === 'ctl' ? 'image_enabled = false\n' : '')).join(''));
  child = spawn(binary, ['--console'], { cwd: dir, stdio: ['pipe','pipe','pipe'] });
  child.stdout.on('data', data => { output += data; }); child.stderr.on('data', data => { output += data; });
  exited = new Promise(resolve => child.once('exit', (code, signal) => resolve({code,signal})));
  await until(() => output.includes('前台控制台已就绪'), 'foreground ready');
  const base = `http://127.0.0.1:${port}`;
  const token = 'test console only';
  const headers = { 'x-acumen-token': token };
  const api = async route => (await fetch(base + '/api' + route, { headers })).json();
  // 隔离：这次启动不该起任何真适配器。落到默认 bot 就会连上本机的 satori-qq。
  assert(!output.includes('启动适配器 [satori]'), 'isolated instance must not start the satori adapter');
  assert.equal((await fetch(base + '/api/logs')).status, 401);
  // 查询串那条载体（地址栏与 EventSource 走它）：口令带空格，必须解码后才比得上。
  assert.equal((await fetch(`${base}/api/logs?limit=1&t=${encodeURIComponent(token)}`)).status, 200);
  // form-urlencoded 那套把空格写成 `+`，与启动日志里打印的地址同一种编码。
  assert.equal((await fetch(`${base}/api/logs?limit=1&t=test+console+only`)).status, 200);
  assert.equal((await fetch(`${base}/api/logs?limit=1&t=nope`)).status, 401);
  // 启动日志里那条地址自己也要能解回口令，否则用户照着它打开就是 401。
  const printed = output.match(/http:\/\/127\.0\.0\.1:\d+\/\?t=\S+/);
  assert(printed, 'startup log prints the console address');
  assert.equal(new URL(printed[0]).searchParams.get('t'), token, 'printed address round-trips the token');
  assert.equal((await api('/logs?limit=1')).lines.length, 1);
  // 配置写 5000，读出侧仍是 2000 那条口径（state::HISTORY_LIMIT）。
  assert((await api('/logs?limit=99999')).lines.length <= 2000, 'history read is capped at 2000');
  abort = new AbortController();
  const stream = await fetch(base + '/api/logs/stream', { headers, signal: abort.signal });
  reader = stream.body.getReader(); let events = [], buffer = '';
  const reading = (async () => {
    try {
      while (true) {
        const {value,done} = await reader.read(); if (done) break;
        buffer += new TextDecoder().decode(value, {stream:true});
        let end;
        while ((end = buffer.indexOf('\n\n')) >= 0) {
          events.push(buffer.slice(0,end)); buffer=buffer.slice(end+2);
        }
      }
    } catch(error) { if (!abort.signal.aborted) throw error; }
  })();
  await until(() => events.some(e=>e.startsWith('event: snapshot')), 'initial SSE snapshot');
  const begin = Date.now();
  child.stdin.write('/ctl list\n');
  await until(() => output.includes('插件状态（全局配置）'), 'foreground responds while WebUI subscribed');
  const foregroundMs = Date.now()-begin;
  const previous = events.length;
  await fetch(base + '/api/command', { method:'POST', headers: {...headers,'content-type':'application/json'}, body:JSON.stringify({input:'set logger debug 开'}) });
  child.stdin.write('/ctl list\n');
  await until(() => events.length > previous, 'SSE still delivers');
  assert.equal((await api('/plugins/logger')).config.debug, true);
  const timings=[];
  for(let i=0;i<10;i++) {const t=performance.now(); await api('/overview'); timings.push(performance.now()-t);}
  abort.abort(); await reading;
  const response=await fetch(base+'/app.js');
  assert.equal(await response.text(), fs.readFileSync(path.join(root,'res/console/app.js'),'utf8'), 'release embeds the exact current JavaScript');
  assert.equal(await (await fetch(base+'/app.css')).text(),
    fs.readFileSync(path.join(root,'res/cards/m3e.css'),'utf8')+'\n'+fs.readFileSync(path.join(root,'res/console/app.css'),'utf8'),
    'release embeds the shared tokens and exact current console stylesheet');
  const etag=response.headers.get('etag');
  assert.equal((await fetch(base+'/app.js',{headers:{'if-none-match':etag}})).status,304);
  child.kill('SIGTERM');
  const result=await Promise.race([exited,sleep(8000).then(()=>{throw new Error('Shutdown hung with stdin open');})]);
  assert.deepEqual(result,{code:0,signal:null});
  console.log(`Console backend passed: isolated (no adapters), auth over header and query, bounded history, snapshot + stream, foreground command ${foregroundMs} ms, config writes, embedded assets, ETag, clean shutdown.`);
  console.log(`Isolated overview request median ${timings.sort((a,b)=>a-b)[5].toFixed(1)} ms (empty test database).`);
})().catch(error=>{console.error(error);process.exitCode=1;}).finally(async()=>{
  abort?.abort();
  if(child && child.exitCode===null && child.signalCode===null) child.kill('SIGKILL');
  if(exited) await exited;
  fs.rmSync(dir,{recursive:true,force:true});
});
