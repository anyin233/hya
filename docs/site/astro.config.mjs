import { defineConfig } from 'astro/config';
import { unified } from '@astrojs/markdown-remark';
import starlight from '@astrojs/starlight';
import remarkDocs from './src/lib/remark-docs.mjs';
import { documentLinks, resolveDocsBase } from './src/lib/paths.mjs';

const base = resolveDocsBase();

export default defineConfig({
  site: process.env.DOCS_SITE || 'https://chrono-foundry.github.io',
  base,
  trailingSlash: 'always',
  publicDir: '../assets',
  markdown: { processor: unified({ remarkPlugins: [[remarkDocs, { base }]] }) },
  integrations: [starlight({
    title: 'hya',
    description: 'AI coding assistant for your terminal, browser, and editor. Guides, reference manuals, extension SDKs, and architecture.',
    logo: { src: '../assets/hya-logo.png', alt: 'hya', replacesTitle: true },
    favicon: '/hya-icon.png',
    social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/chrono-foundry/hya' }],
    components: { MarkdownContent: './src/components/MarkdownContent.astro' },
    expressiveCode: { shiki: { langAlias: { 'rust,ignore': 'rust', caddyfile: 'text' } } },
    credits: true,
    sidebar: [
      { label: 'Home', slug: 'index' },
      {
        label: 'Get Started',
        items: [
          { label: 'What is Hya?', slug: 'getting-started/overview' },
          { label: 'Installation', slug: 'getting-started/installation' },
          { label: '5-Minute Quickstart', slug: 'getting-started/quickstart' },
          { label: 'Common Workflows', slug: 'getting-started/common-workflows' },
        ],
      },
      {
        label: 'Guides',
        items: [
          { label: 'Interactive Mode (TUI & WebUI)', slug: 'guide/interactive-mode' },
          { label: 'Editor Integration (ACP)', slug: 'guide/editor-integration' },
          { label: 'Headless & Automation', slug: 'guide/headless-and-automation' },
          { label: 'Models, Providers & Auth', slug: 'guide/models-and-providers' },
          { label: 'Projects, Permissions & Safety', slug: 'guide/projects-and-permissions' },
          { label: 'Context, Memory & Compaction', slug: 'guide/context-and-memory' },
          { label: 'Subagents & Multi-Agent Teams', slug: 'guide/subagents-and-teams' },
          { label: 'Workflows', slug: 'guide/workflows' },
          { label: 'Skills, Commands & MCP', slug: 'guide/skills-commands-and-mcp' },
          { label: 'Background Server & Remote Relay', slug: 'guide/daemon-and-relay' },
        ],
      },
      {
        label: 'Reference',
        items: [
          { label: 'CLI Reference', slug: 'reference/cli' },
          { label: 'Configuration Reference', slug: 'reference/configuration' },
          { label: 'Slash Commands & Keybindings', slug: 'reference/commands-and-keybindings' },
          { label: 'Tools, Agents & Bundles', slug: 'reference/tools-and-presets' },
          { label: 'Environment & File Paths', slug: 'reference/env-and-paths' },
          { label: 'Troubleshooting', slug: 'reference/troubleshooting' },
        ],
      },
      {
        label: 'Extending Hya',
        collapsed: true,
        items: [
          { label: 'Overview', slug: 'extend/overview' },
          { label: 'Skills & Slash Commands', slug: 'extend/skills-and-commands' },
          { label: 'Authoring Workflows & Evaluators', slug: 'extend/authoring-workflows' },
          { label: 'Creating Bundles (.hyabundle)', slug: 'extend/bundles' },
          { label: 'hyabundle Specification', slug: 'extend/hyabundle-spec' },
          { label: 'Plugins, Tools & Hooks', slug: 'extend/plugins-and-hooks' },
          { label: 'TUI Extensions (@hya/tui-sdk)', slug: 'extend/tui-extensions' },
        ],
      },
      {
        label: 'Protocol & SDK',
        collapsed: true,
        items: [
          { label: 'hya.v1 Protocol Overview', slug: 'protocol/index' },
          { label: 'SDK & Client Integration', slug: 'protocol/sdk' },
          { label: 'Generated API Reference', slug: 'protocol/api-reference' },
        ],
      },
      {
        label: 'Architecture',
        collapsed: true,
        items: [
          { label: 'Architecture Overview', slug: 'architecture/overview' },
          { label: 'Repository & Crate Map', slug: 'architecture/codebase-map' },
          { label: 'Kernel & Context Engine', slug: 'architecture/kernel-and-context' },
          { label: 'Event Model & Storage', slug: 'architecture/events-and-storage' },
          { label: 'Session Engine & Subagents', slug: 'architecture/engine-and-subagents' },
          { label: 'Providers, Tool Plane & Scoping', slug: 'architecture/providers-and-tools' },
          { label: 'Server, Relay & Frontend', slug: 'architecture/server-relay-and-tui' },
          { label: 'Domain Glossary', slug: 'architecture/glossary' },
        ],
      },
      {
        label: 'Contributing',
        collapsed: true,
        items: [
          { label: 'Development & Coding Standards', slug: 'contributing/development' },
          { label: 'Plan Template', slug: 'templates/plan' },
          { label: 'Testing & Verification', slug: 'contributing/testing' },
          { label: 'Dev Tooling & Docs Site', slug: 'contributing/tooling-and-docs-site' },
          { label: 'Versioning & Release Guide', slug: 'contributing/release' },
        ],
      },
      { label: 'Architecture Decisions (ADR)', collapsed: true, items: documentLinks('adr') },
    ],
  })],
});
