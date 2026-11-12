# When are project bundle source bytes scanned?

First binding, changed Project roots, eviction/rebinding, and explicit
`RefreshBundles` scan sources. Repeated binding uses the remembered bundle
input digest and prepared records; it still checks project plugin manifests.
Empty Projects retain an empty overlay too, so their scan state follows the
existing scope eviction limits and adding source files requires refresh.
Installed bundle configuration bytes are also read only on explicit refresh;
registry generations still trigger automatic publication.
The explicit refresh contract is already available at
`POST /v1/bundles:refresh` with `directory`. Bundle management invokes it;
project Agent model saving now invokes it too so a save applies immediately.
The Linux regression watches actual manifest and Skill file access with
inotify: it failed before caching and requires no access events on three binds.

# Does Skill discovery hold the publication lock?

`RuntimeRegistry::bind_scoped_with_skills` discovers first, then acquires the
publication mutex and compares/publishes the prepared data. Its test checks
`try_lock` from the discovery callback. Retired snapshots and replaced scope
sources are released after the publication lock too, because owner destruction
can remove materialized files. Candidate refresh closures must prepare I/O
before entering registry publication.
