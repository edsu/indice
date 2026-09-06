// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// Published on GitHub Pages under the custom domain https://indice.inkdroid.org/
// (set in Settings → Pages), so the site is served from the root — no base path.
// The custom landing page (src/pages/index.astro) owns `/`; the Starlight manual
// lives under `/docs/` because its content is nested in src/content/docs/docs/**.
export default defineConfig({
  site: 'https://indice.inkdroid.org',
  integrations: [
    starlight({
      title: 'indice',
      favicon: '/favicon.svg',
      // Same Plausible analytics as the landing page (src/pages/index.astro);
      // Starlight renders its own <head>, so the tags have to be declared here.
      head: [
        {
          tag: 'script',
          attrs: { src: 'https://plausible.io/js/pa-lPSq4HYaeHRGZU-nYOYio.js', async: true },
        },
        {
          tag: 'script',
          content:
            'window.plausible=window.plausible||function(){(plausible.q=plausible.q||[]).push(arguments)},plausible.init=plausible.init||function(i){plausible.o=i||{}};plausible.init()',
        },
      ],
      customCss: ['./src/styles/starlight-theme.css'],
      components: {
        ThemeProvider: './src/components/ThemeProvider.astro',
        ThemeSelect: './src/components/ThemeSelect.astro',
      },
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/edsu/indice' },
      ],
      sidebar: [
        { label: '← Back to indice', link: '/' },
        {
          label: 'Start here',
          items: [
            { label: 'Introduction', slug: 'docs' },
            'docs/install',
            'docs/quickstart',
          ],
        },
        {
          label: 'Guides',
          items: [
            'docs/guides/searching',
            'docs/guides/import-browsertrix',
            'docs/guides/import-archive-it',
            'docs/guides/manage',
            'docs/guides/deploy',
            'docs/guides/scale',
          ],
        },
        {
          label: 'Reference',
          items: [
            'docs/reference/cli',
            'docs/reference/api',
            'docs/reference/home-directory',
            'docs/reference/configuration',
            'docs/reference/how-it-works',
          ],
        },
        {
          label: 'Contributing',
          items: [
            'docs/contributing/building-and-testing',
            'docs/contributing/benchmarking',
          ],
        },
      ],
    }),
  ],
});
