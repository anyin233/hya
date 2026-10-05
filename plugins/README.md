# Standalone plugins

This directory holds optional plugins developed independently from the harness.
Each plugin owns its bundle manifest, backend provider, frontend contributions,
contracts, documentation, and tests. Installation uses the existing `.hyabundle`
format; the directory name does not introduce another loader.

- [Disk inspector](disk-inspector/README.md): the first frontend/backend plugin
  scaffold. Backend discovery works; disk scanning and the generic frontend API
  bridge are subsequent steps.

Plugins here are not automatically installed or included in release archives.
Keep packaged runtime imports inside the plugin, except for the public TUI SDK
provided by the frontend host. Development can resolve that SDK from this checkout.
