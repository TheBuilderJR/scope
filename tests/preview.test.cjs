const { test } = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync(`${__dirname}/../src/app.js`, 'utf8');

function setup() {
  const released = [];
  const media = { pause: () => released.push('pause'), removeAttribute: () => released.push('remove'), load: () => released.push('load') };
  const frame = { src: 'external.pdf' };
  const previewEl = {
    innerHTML: 'old preview', classList: { toggle() {} },
    querySelectorAll: selector => selector === 'iframe' ? [frame] : [media],
  };
  const context = vm.createContext({
    previewEl, previewVisible: true, previewGeneration: 0,
    selectedPath: '/drive/file', selectedPaths: new Set(['/drive/file']),
    currentEntries: [], folderSizeCache: new Map(), fmtBytes: String,
    togglePreviewEl: { setAttribute() {} }, savePref() {},
    updateSelectionPreview() {}, escapeHtml: String,
    invoke: () => { throw new Error('unexpected backend call'); },
  });
  for (const [start, end] of [
    ['function applyPreviewState()', 'togglePreviewEl.addEventListener'],
    ['function clearPreview()', '// A large, crisp QuickLook'],
    ['async function showPreview(entry)', '// ---- View mode toggle'],
    ['function showMultiPreview()', 'function openEntry(entry)'],
  ]) vm.runInContext(source.slice(source.indexOf(start), source.indexOf(end, source.indexOf(start))), context);
  return { context, released, frame, previewEl };
}

test('collapsing unloads media and reopening refreshes the selected preview', () => {
  const { context, released, frame } = setup();
  let refreshed = 0;
  context.updateSelectionPreview = () => refreshed++;
  context.togglePreview();
  assert.equal(context.previewVisible, false);
  assert.deepEqual(released, ['pause', 'remove', 'load']);
  assert.equal(frame.src, 'about:blank');
  context.togglePreview();
  assert.equal(refreshed, 1);
});

test('hidden preview never reads the selected file', async () => {
  const { context } = setup();
  context.previewVisible = false;
  await context.showPreview({ path: context.selectedPath });
});

for (const reject of [false, true]) {
  test(`late preview ${reject ? 'error' : 'result'} cannot replace a multi-selection`, async () => {
    const { context, previewEl, released } = setup();
    let finish;
    context.invoke = () => new Promise((resolve, fail) => { finish = reject ? fail : resolve; });
    const request = context.showPreview({ path: context.selectedPath });
    context.showMultiPreview();
    const summary = previewEl.innerHTML;
    finish(reject ? new Error('unmounted') : {});
    await request;
    assert.equal(previewEl.innerHTML, summary);
    assert.equal(released.filter(action => action === 'load').length, 2);
  });
}
