/**
 * Transcript scroll following. OpenTUI's scrollbox keeps a sticky bottom by
 * itself (`stickyScroll`, `stickyStart: "bottom"`): it follows new content
 * while the view is at the bottom and stops once the user scrolls up.
 * `ScrollFollow` adds the "new messages below" hint on top of that: it raises
 * the hint when content grows below a scrolled-up view and clears it at the
 * bottom. Pure; the Transcript component feeds it the scrollbox metrics once
 * per rendered frame.
 */

export interface ScrollMetrics {
  scrollTop: number
  scrollHeight: number
  viewportHeight: number
}

/** The last content row is in view (one row of slack for rounding). */
export function atBottom({ scrollTop, scrollHeight, viewportHeight }: ScrollMetrics): boolean {
  return scrollTop >= Math.max(0, scrollHeight - viewportHeight) - 1
}

/** Rows moved by PgUp/PgDn: a viewport, keeping two rows of context. */
export function pageStep(viewportHeight: number): number {
  return Math.max(1, viewportHeight - 2)
}

export class ScrollFollow {
  private following = true
  private top: number | undefined
  private height = 0
  private unseen = false

  observe(metrics: ScrollMetrics): { atBottom: boolean; unseen: boolean } {
    const bottom = atBottom(metrics)
    if (bottom) this.unseen = false
    else if (!this.following && metrics.scrollHeight > this.height) this.unseen = true
    // Content that grew under a following view (scrollTop unchanged) is not a
    // user scroll: the sticky bottom catches up on the next layout.
    this.following = bottom || (this.following && metrics.scrollTop === this.top)
    this.top = metrics.scrollTop
    this.height = metrics.scrollHeight
    return { atBottom: bottom, unseen: this.unseen }
  }

  reset(): void {
    this.following = true
    this.top = undefined
    this.height = 0
    this.unseen = false
  }
}
