const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(require('node:path').join(__dirname, '../res/console/app.js'), 'utf8');
const fn = name => {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
};
(async () => {
  let release, requests = [];
  const attrs = new Map();
  const control = { isConnected: true, value: 'first', defaultValue: 'old', dataset: { kind: 'string', path: 'prompt' },
    closest: () => ({dataset: {config: 'oai'}}), getAttribute: key => attrs.get(key),
    setAttribute: (key, value) => attrs.set(key, value), removeAttribute: key => attrs.delete(key) };
  const context = vm.createContext({ api: async (_path, request) => { requests.push(request.body); await new Promise(resolve => release = resolve); return {message: 'saved'}; },
    clearFieldError() {}, fieldError() {}, snackbar() {}, refreshDiff() {}, report() {} });
  vm.runInContext(fn('parseValue') + fn('commitField'), context);
  const pending = context.commitField(control);
  control.value = 'second';
  await context.commitField(control);
  release(); await pending;
  assert.equal(requests.length, 2);
  assert.equal(control.defaultValue, 'first');
  assert.equal(requests[1].value, 'second');
  release(); await new Promise(resolve => setImmediate(resolve));
  assert.equal(control.defaultValue, 'second');
  assert.deepEqual(JSON.parse(JSON.stringify(context.parseValue('list', '["00123","true","a,b",{"enabled":false}]'))), ['00123','true','a,b',{enabled:false}]);
  assert.throws(() => context.parseValue('list', '1,2,3'));
  // 名单编辑器：从一段文字里拆群号。引号、括号、分隔符、全角、零宽字符都不该成为群号的一部分，也不能丢字。
  const shared = [...source.matchAll(/^  const (?:ID_SPLIT|ID_EDGE|ID_SHAPE) = .*;$/gm)].map(match => match[0]).join('\n');
  assert.equal(shared.split('\n').length, 3, '三个解析常量都取到了');
  vm.runInContext(shared + fn('parseIds'), context);
  const ids = text => JSON.parse(JSON.stringify(context.parseIds(text)));
  assert.deepEqual(ids('["123", "456"]'), { ids: ['123', '456'], rejected: [] }, '整段 JSON');
  assert.deepEqual(ids(' 123，456、789;\n012 '), { ids: ['123', '456', '789', '012'], rejected: [] }, '前导零保留，各种分隔');
  assert.deepEqual(ids('１２３\u3000４５６'), { ids: ['123', '456'], rejected: [] }, '全角数字与全角空格');
  assert.deepEqual(ids('12345@chatroom, wxid_abc, gh_3dfda90e39d6'), { ids: ['12345@chatroom', 'wxid_abc', 'gh_3dfda90e39d6'], rejected: [] }, '微信的群与账号');
  assert.deepEqual(ids('白虎 123'), { ids: ['123'], rejected: ['白虎'] }, '认不出的单独报出来');
  assert.deepEqual(ids('"123","123",「123」'), { ids: ['123'], rejected: [] }, '去重');
  assert.deepEqual(ids('\u200b123\ufeff'), { ids: ['123'], rejected: [] }, '零宽字符');
  assert.deepEqual(ids('-1001'), { ids: ['-1001'], rejected: [] }, '负号开头也是合法 ID');
  assert.deepEqual(ids(' , ， '), { ids: [], rejected: [] }, '只有分隔符');
  assert.deepEqual(ids('a'.repeat(129)), { ids: [], rejected: ['a'.repeat(129)] }, '过长的不收');
  const ambient = { data: {persona: 'old'}, drafts: {persona: 'first'} };
  const button = {dataset: {saveSource:'persona'}, disabled:false, getAttribute:key=>attrs.get(key), setAttribute:(key,value)=>attrs.set(key,value), removeAttribute:key=>attrs.delete(key)};
  Object.assign(context, {ambient, sourceText:name=>ambient.drafts[name], dirty:name=>ambient.drafts[name] !== undefined && ambient.drafts[name] !== ambient.data[name], editorState:()=>'', $:()=>({textContent:'',toggleAttribute(){}})});
  vm.runInContext(fn('saveSource'), context);
  const saving = context.saveSource(button);
  ambient.drafts.persona = 'second'; release(); await saving;
  assert.equal(ambient.data.persona, 'first');
  assert.equal(ambient.drafts.persona, 'second');
  assert.equal(button.disabled, false);
  context.$ = () => null;
  context.report = () => { throw new Error('Navigating away must not turn a successful save into an error'); };
  const afterNavigation = context.saveSource(button);
  release(); await afterNavigation;
  assert.equal(ambient.data.persona, 'second');
  console.log('PASS: in-flight edits, source drafts, navigation during save, lossless arrays, group-id parsing');
})().catch(error=>{console.error(error);process.exitCode=1;});
