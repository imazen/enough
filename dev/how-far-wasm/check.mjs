// Node's engine exercises real Wasm stack suspension; this is not a Safari test.
import { readFile } from 'node:fs/promises';
import assert from 'node:assert/strict';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';

if (!isMainThread) {
  const { instance } = await WebAssembly.instantiate(workerData, {
    host: { checkpoint(n) { parentPort.postMessage({ progress: n }); return n >= 40 ? 1 : 0; } },
  });
  parentPort.postMessage({ completed: instance.exports.run(100) });
} else {
  const bytes = await readFile(new URL('./target/wasm32-unknown-unknown/debug/how_far_wasm_probe.wasm', import.meta.url));
  const progress = [];
  let scheduled = 0;
  const ordinary = await WebAssembly.instantiate(bytes, {
    host: { checkpoint(n) { progress.push(n); setTimeout(() => scheduled++, 0); return 0; } },
  });
  assert.equal(ordinary.instance.exports.run(100), 100);
  assert.equal(scheduled, 0, 'ordinary sync callbacks cannot service timer tasks');
  assert.deepEqual(progress, Array.from({ length: 101 }, (_, i) => i));
  await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(scheduled, 101);

  assert.equal(typeof WebAssembly.Suspending, 'function', 'use a JSPI-capable Node');
  let turns = 0;
  const suspended = await WebAssembly.instantiate(bytes, {
    host: { checkpoint: new WebAssembly.Suspending(async n => {
      if (n % 10 === 0) {
        await new Promise(resolve => setTimeout(() => { turns++; resolve(); }, 0));
      }
      return n >= 40 ? 1 : 0;
    }) },
  });
  assert.equal(await WebAssembly.promising(suspended.instance.exports.run)(100), 40);
  assert.equal(turns, 5, 'timer tasks execute while the synchronous Rust stack is suspended');

  const messages = [];
  const worker = new Worker(new URL(import.meta.url), { workerData: bytes });
  await new Promise((resolve, reject) => {
    worker.on('message', message => messages.push(message));
    worker.on('error', reject);
    worker.on('exit', code => code === 0 ? resolve() : reject(new Error(`worker exit ${code}`)));
  });
  assert.deepEqual(messages.at(-1), { completed: 40 });
  assert.equal(messages.filter(m => 'progress' in m).length, 41);
  console.log('Passed: synchronous timer boundary, JSPI yield/resume/cancel, worker posted progress.');
}
