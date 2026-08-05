// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// The docs deploy to GitHub Pages at the project path
// https://sumanyumuku98.github.io/MemMux/ (base = "/MemMux"). If a custom domain is added later,
// set `site` to it and `base: '/'` so the site serves at the domain root.
const site = 'https://sumanyumuku98.github.io';
const base = '/MemMux';

export default defineConfig({
  site,
  base,
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'MemMux',
      description: 'The memory-aware local runtime for parallel AI coding agents.',
      logo: { src: './src/assets/icon.png', alt: 'MemMux' },
      favicon: '/favicon.png',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/sumanyumuku98/MemMux' },
      ],
      editLink: {
        baseUrl: 'https://github.com/sumanyumuku98/MemMux/edit/main/website/',
      },
      lastUpdated: true,
      sidebar: [
        { label: 'Introduction', link: '/' },
        { label: 'Installation', link: '/install/' },
        {
          label: 'Design',
          items: [
            { label: 'Architecture', link: '/design/architecture/' },
            { label: 'Threat model', link: '/design/threat-model/' },
            { label: 'Benchmark methodology', link: '/design/benchmark-methodology/' },
          ],
        },
        {
          label: 'Phases',
          items: [
            { label: 'Phase 0 — Instrumentation', link: '/phases/phase-0/' },
            { label: 'Phase 1 — Multiplexer', link: '/phases/phase-1/' },
            { label: 'Phase 2 — Lifecycle runtime', link: '/phases/phase-2/' },
          ],
        },
        {
          label: 'Operations',
          items: [{ label: 'Releasing', link: '/operations/releasing/' }],
        },
        {
          label: 'Reference',
          // The rustdoc API reference is copied to /api/ at deploy time; Starlight prepends the
          // site base to this internal link.
          items: [{ label: 'API reference (rustdoc)', link: '/api/' }],
        },
      ],
    }),
  ],
});
