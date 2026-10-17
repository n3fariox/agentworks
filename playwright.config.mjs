import {defineConfig} from '@playwright/test';
export default defineConfig({
  testDir:'./tests',workers:1,timeout:30000,
  use:{baseURL:process.env.AGENTWORKS_URL||'http://127.0.0.1:4317',headless:true},
  outputDir:'.cache/browser-results',reporter:'list',
});
