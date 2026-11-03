/// <reference types="node" />
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  server: {
    host: '127.0.0.1',
    port: 5173,
    // QUIPCALL_API lets a second dev stack (e.g. for automated tests) run alongside.
    proxy: { '/api': process.env.QUIPCALL_API ?? 'http://127.0.0.1:8080' },
  },
});
