# Why was the first remote Project key lost?

CI 38294165613 showed the Projects list after n instead of its New project name form. enterServer started onScopeChanged without awaiting its catalog request and extension reload. The old local project_view could render and receive input before manager.load replaced it: local first-party extensions use trusted execution while remote ones do not. Await the refresh before opening Projects or announcing Connected.

The unit regression blocks the refresh promise and verifies that Connected cannot appear until it resolves. It failed before the controller fix. Keep testing the real relay connection and first n key in the shared terminal spec.
