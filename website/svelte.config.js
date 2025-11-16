import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),

  kit: {
    adapter: adapter({
      pages: 'build',
      assets: 'build'
    }),

    // Use import.meta.env.MODE instead of process.env.NODE_ENV to avoid errors
    paths: {
      base: '/stasis'
    },

    prerender: {
      handleHttpError: 'ignore' // optional: silence 404s during prerender
    }
  }
};

export default config;
