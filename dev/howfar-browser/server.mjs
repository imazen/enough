import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { resolve, extname } from 'node:path';
const root = resolve(import.meta.dirname);
createServer(async (request, response) => {
  const path = resolve(root, `.${new URL(request.url, 'http://localhost').pathname}`);
  response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  if (new URL(request.url, 'http://localhost').pathname === '/raw.wasm') {
    response.setHeader('Content-Type', 'application/wasm');
    try { response.end(await readFile(resolve(root, '../howfar-wasm/target/wasm32-unknown-unknown/debug/howfar_wasm_probe.wasm'))); }
    catch { response.writeHead(404).end(); }
    return;
  }
  if (path === root) {
    response.setHeader('Content-Type', 'text/html');
    response.end('<!doctype html><title>howfar browser tests</title><p id="progress">waiting</p>');
    return;
  }
  if (!path.startsWith(root + '/')) { response.writeHead(403).end(); return; }
  try {
    response.setHeader('Content-Type', ({ '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm' })[extname(path)] ?? 'application/octet-stream');
    response.end(await readFile(path));
  } catch { response.writeHead(404).end(); }
}).listen(4179, '127.0.0.1');
