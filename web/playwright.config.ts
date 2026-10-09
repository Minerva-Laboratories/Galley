import { defineConfig } from '@playwright/test';

// scripts/e2e.sh builds the app, starts `galley serve` on this port, and runs these tests.
const base = process.env.GALLEY_E2E_URL ?? 'http://127.0.0.1:7411';

export default defineConfig({
  testDir: './e2e',
  timeout: 60_000,
  // On CI, one retry keeps a timing flake from blocking the team, and the github reporter still
  // records it: failures and flaky tests become run annotations, which anyone can read through the
  // API, while the logs need admin rights. Fix every test the annotations call flaky.
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['github']] : 'list',
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
