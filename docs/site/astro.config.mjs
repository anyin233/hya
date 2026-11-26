import { defineConfig } from 'astro/config';
import { unified } from '@astrojs/markdown-remark';
import starlight from '@astrojs/starlight';
import remarkDocs from './src/lib/remark-docs.mjs';
import { documentLinks } from './src/lib/paths.mjs';

const base = process.env.DOCS_BASE || '/';

export default defineConfig({
  site: process.env.DOCS_SITE || 'https://chrono-foundry.github.io',
  base,
  trailingSlash: 'always',
  publicDir: '../assets',
  markdown: { processor: unified({ remarkPlugins: [[remarkDocs, { base }]] }) },
  integrations: [starlight({
    title: 'hya',
    description: 'An event-sourced, multi-agent coding runtime. Architecture, guides, and API reference.',
    logo: { src: '../assets/hya-icon-8bit.png', alt: 'hya' },
    favicon: '/hya-icon-8bit.png',
    social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/chrono-foundry/hya' }],
    components: { MarkdownContent: './src/components/MarkdownContent.astro' },
    expressiveCode: { shiki: { langAlias: { 'rust,ignore': 'rust', caddyfile: 'text' } } },
    credits: true,
    sidebar: [
      { label: 'Overview', slug: 'index' },
      {
        label: 'Start here',
        items: [
          { label: 'Quick start', slug: 'getting-started' },
          { label: 'Install & update', slug: 'install' },
          { label: 'Windows / WSL', slug: 'install-wsl' },
          { label: 'Configuration', slug: 'configuration' },
          { label: 'CLI reference', slug: 'cli' },
          { label: 'Troubleshooting', slug: 'troubleshooting' },
        ],
      },
      {
        label: 'Using hya',
        items: ['tui', 'tui-web', 'acp', 'relay', 'skills', 'workflows', 'goal-loop-authoring', 'compaction']
          .map((slug) => ({ slug })),
      },
      { label: 'Architecture', items: documentLinks('architecture') },
      { label: 'Project structure', slug: 'project-structure' },
      {
        label: 'Bundles & extensions', collapsed: true,
        items: ['agent-bundle-authoring', 'bundle-runtime', 'core-agents', 'base-tools',
          'subagent-bundles', 'agent-channels', 'extra-bundles', 'code-mode',
          'claude-plugin-import', 'plugin-protocol', 'tui-extensions', 'tui-extension-sdk', 'disk-inspector']
          .map((slug) => ({ slug })),
      },
      { label: 'Protocol & API', items: documentLinks('protocol') },
      {
        label: 'Contributing', collapsed: true,
        items: [
          { slug: 'development' },
          { label: 'Plan template', slug: 'templates/plan' },
          { slug: 'docs-site' },
          { label: 'Testing', items: documentLinks('testing') },
          { label: 'Guidelines', items: documentLinks('spec') },
          { label: 'Agent references', items: documentLinks('agents') },
        ],
      },
      { label: 'Architecture decisions', collapsed: true, items: documentLinks('adr') },
    ],
  })],
});
