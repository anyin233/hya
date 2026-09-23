# 0.39.0

## `hya bundle`: scopes, confirmation, and verify

- **Breaking:** `hya bundle install` and `hya bundle remove` now show a summary and ask `Proceed? [y/N]` before changing anything. Pass `-y`/`--yes` to skip the prompt. Closed or empty stdin cancels (exit 1), so scripts and CI jobs that omit `-y` fail safely instead of hanging. An install whose content is already present prints `unchanged` without asking.
- New `--user` (default) and `--project` scope flags on `install`, `remove`, `verify`, `list`, and `info`. The project scope is `./.hya/bundles`, the directory the runtime already loads project bundles from. A project install unpacks the package's declared source files into `.hya/bundles/<id with / replaced by __>/` through a staging directory. A project remove deletes the bundle's directory. Both scopes share the same conflict rules. In the project scope, `--overwrite` can also replace a same-version edit.
- `hya bundle remove` is the canonical name. `uninstall` stays as an alias. Output is now `removed <id> scope=user generation=<n>` or `removed <id> scope=project path=<dir>`.
- New `hya bundle verify <PACKAGE>` runs every install check against a scope (package integrity, preset and first-party rules, reserved Agent ids, downgrade, namespace, and content conflicts) and prints what `install` would do. It writes nothing: no registry and no `./.hya`.
- `hya bundle list` gains a SCOPE column (`builtin`, `user`, `project`) and a `shadowed` state for user bundles hidden by a project bundle, matching what the runtime loads. `search` rows gain the same column.
- `hya bundle info` also accepts a package file as its positional argument, finds project bundles (`origin=project`, `path=`), and prints a `scope=` line.
- `install` output gains `scope=user` or `scope=project path=<dir>`.
- Fix: `install` and `remove` open the user registry once per command. Back-to-back connections could race the previous pool's shutdown and fail with `database is locked`.
- Library: `BundleRegistry::plan_install` is a dry run of `install` that shares its validation. `PublicPackageInspection::files` returns a package's verified source files. `hya_app::project_bundles` gains project install, plan, find, and remove functions.
