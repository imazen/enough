import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.',
  testMatch: 'browser.spec.mjs',
  timeout: 60_000,
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:4179' },
  projects: [
    { name: 'chromium', use: { browserName: 'chromium' } },
    { name: 'webkit', use: { browserName: 'webkit' } },
  ],
  webServer: { command: 'node server.mjs', port: 4179, reuseExistingServer: false },
});
