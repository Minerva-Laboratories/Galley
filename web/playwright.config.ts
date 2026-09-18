import { defineConfig } from '@playwright/test';

// scripts/e2e.sh builds the app, starts `galley serve` on this port, and runs these tests.
const base = process.env.GALLEY_E2E_URL ?? 'http://127.0.0.1:7411';

export default defineConfig({
  testDir: './e2e',
  timeout: 60_000,
  retries: 0,
  use: {
    baseURL: base,
    trace: 'retain-on-failure',
  },
  // GALLEY_E2E_CHANNEL=chrome uses an installed Google Chrome instead of the downloaded Chromium.
  projects: [
    {
      name: 'chromium',
      use: { browserName: 'chromium', channel: process.env.GALLEY_E2E_CHANNEL || undefined },
    },
  ],
});
