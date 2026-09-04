// @ts-check
import { defineConfig } from 'astro/config';

// Published as a GitHub Pages *project* site at https://edsu.github.io/indice/,
// so everything is served under the `/indice` base path. Use import.meta.env.BASE_URL
// (or Astro's asset pipeline) for links/assets so they resolve correctly.
export default defineConfig({
  site: 'https://edsu.github.io',
  base: '/indice',
});
