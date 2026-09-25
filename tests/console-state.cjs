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
  console.log('PASS: in-flight edits, source drafts, navigation during save, lossless arrays');
})().catch(error=>{console.error(error);process.exitCode=1;});
