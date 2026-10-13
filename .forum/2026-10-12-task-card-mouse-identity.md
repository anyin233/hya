# Why can clicking a displayed task child do nothing?

A native terminal mouse frame targets the renderable IDs from its last draw.
Recreating tool-card subtrees on a projection refresh can remove those IDs before
the user clicks the still-visible frame. CPU-constrained repetitions reproduced
the CI failure; tracing confirmed the mouse target ID no longer existed.

Keep the built-in fallback mounted while no extension supplies a replacement or
decorator. Key task rows by child session, then member handle, rather than fresh
object identity. The regression replaces both roster and transcript data and
checks native renderable identity plus actual mouse navigation. A removed node
can remain temporarily undestroyed, so `isDestroyed` alone is insufficient.

Twenty repeated real-PTY batch scenarios on two cores passed after the fix.
