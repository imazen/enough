import { test, expect } from '@playwright/test';

test('main-thread host yields and receives cancellation with native suspension or chunks', async ({ page }) => {
  await page.goto('/');
  const result = await page.evaluate(async () => {
    const bytes = await (await fetch('/raw.wasm')).arrayBuffer();
    let turns = 0;
    let cancelled = false;
    let heartbeat = 0;
    const timer = setInterval(() => heartbeat++, 1);
    const yieldToHost = () => new Promise(resolve => setTimeout(() => {
      turns++;
      if (turns === 3) cancelled = true;
      document.querySelector('#progress').textContent = `UI turn ${turns}`;
      resolve();
    }, 0));
    try {
      if (typeof WebAssembly.Suspending === 'function' && typeof WebAssembly.promising === 'function') {
        const { instance } = await WebAssembly.instantiate(bytes, {
          host: { checkpoint: new WebAssembly.Suspending(async n => {
            if (n % 10 === 0) await yieldToHost();
            return cancelled ? 1 : 0;
          }) },
        });
        const completed = await WebAssembly.promising(instance.exports.run)(100);
        return { mode: 'jspi', completed, turns, heartbeat, cancelled };
      }
      const { instance } = await WebAssembly.instantiate(bytes, { host: { checkpoint: () => 0 } });
      instance.exports.begin_chunks(100);
      while (!cancelled) {
        instance.exports.run_chunk(10, 0);
        await yieldToHost();
      }
      const completed = instance.exports.run_chunk(10, 1);
      return { mode: 'chunks', completed, turns, heartbeat, cancelled: instance.exports.chunks_cancelled() === 1 };
    } finally { clearInterval(timer); }
  });
  expect(result.turns).toBe(3);
  expect(result.heartbeat).toBeGreaterThan(0);
  expect(result.cancelled).toBe(true);
  expect(result.completed).toBe(result.mode === 'jspi' ? 20 : 30);
  await expect(page.locator('#progress')).toHaveText('UI turn 3');
});

for (const cancel of [false, true]) {
  test(`worker-owned Rayon pool: ${cancel ? 'UI observes and cancels shared work' : 'serial/parallel/join/serial completes'}`, async ({ page }) => {
    await page.goto('/');
    const result = await page.evaluate(async shouldCancel => {
      if (!crossOriginIsolated) throw new Error('shared-memory headers missing');
      const worker = new Worker('/worker.mjs', { type: 'module' });
      const ready = await new Promise((resolve, reject) => {
        worker.onmessage = event => { if (event.data.type === 'ready') resolve(event.data); };
        worker.onerror = reject;
      });
      // Instantiate a second binding context over the same module and memory.
      // All reads/cancellation below happen on the browser's actual UI thread.
      const api = await import('/pkg/howfar_browser_probe.js');
      await api.default({ module_or_path: ready.module, memory: ready.memory });
      let ticks = 0;
      let sawRunning = false;
      let requested = false;
      const samples = [];
      const timer = setInterval(() => {
        ticks++;
        api.trace(); // Exercise the profiler's nonblocking UI path too.
        const sample = api.observe();
        if (sample === undefined) return; // Retry a busy metadata read on the next UI turn.
        const root = JSON.parse(sample).root;
        samples.push(root);
        document.querySelector('#progress').textContent = String(root.fraction);
        if (root.children.length && root.children[1].status === 'Running') {
          sawRunning = true;
          if (shouldCancel && !requested && BigInt(root.children[1].completed) > 0n) {
            requested = true;
            api.cancel();
          }
        }
      }, 1);
      const done = new Promise((resolve, reject) => {
        worker.onmessage = event => { if (event.data.type === 'done') resolve(event.data.snapshot); };
        worker.onerror = reject;
      });
      worker.postMessage({ type: 'run', items: shouldCancel ? 10_000_000 : 100_000 });
      try {
        const snapshot = await done;
        const frozen = api.observe();
        await new Promise(resolve => setTimeout(resolve, 5));
        if (api.observe() !== frozen) throw new Error('terminal snapshot changed');
        return { snapshot, trace: JSON.parse(api.trace()), ticks, sawRunning, requested, sampleCount: samples.length };
      } finally { clearInterval(timer); worker.terminate(); }
    }, cancel);
    expect(result.ticks).toBeGreaterThan(0);
    expect(result.trace.spans).toHaveLength(1);
    expect(result.trace.spans[0].kind).toBe('Wait');
    expect(result.snapshot.root.status).toBe('Finished');
    expect(result.snapshot.root.children[0].outcome).toBe('Succeeded');
    expect(result.snapshot.root.children[1].execution).toBe('WorkPool');
    expect(result.snapshot.root.children[1].max_parallelism).toBe(4);
    if (cancel) {
      expect(result.sawRunning && result.requested).toBe(true);
      expect(result.snapshot.root.outcome).toBe('Cancelled');
      expect(BigInt(result.snapshot.root.children[1].completed)).toBeLessThan(10_000_000n);
      expect(result.snapshot.root.children[2].completed).toBe('0');
    } else {
      expect(result.snapshot.root.outcome).toBe('Succeeded');
      expect(result.snapshot.root.fraction).toBe(1);
      expect(result.snapshot.root.children[1].completed).toBe('100000');
      expect(result.snapshot.root.children[2].completed).toBe('1');
    }
  });
}
