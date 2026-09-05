// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// Published as a GitHub Pages *project* site at https://edsu.github.io/indice/,
// so everything is served under the `/indice` base path. The custom landing page
// (src/pages/index.astro) owns `/`; the Starlight manual lives under `/docs/`
// because its content is nested in src/content/docs/docs/**.
export default defineConfig({
  site: 'https://edsu.github.io',
  base: '/indice',
  integrations: [
    starlight({
      title: 'indice',
      favicon: '/favicon.svg',
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
