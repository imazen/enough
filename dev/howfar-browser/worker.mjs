import init, { initThreadPool, prepare, run } from './pkg/howfar_browser_probe.js';

const exports = await init();
await initThreadPool(4);
prepare();
postMessage({ type: 'ready', module: init.__wbindgen_wasm_module, memory: exports.memory });
onmessage = event => {
  if (event.data.type === 'run') {
    postMessage({ type: 'done', snapshot: JSON.parse(run(event.data.items)) });
  }
};
