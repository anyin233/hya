import { createMemo } from "solid-js"
import { useSync } from "../context/sync"
import { hasLiveCatalogModels } from "../../hya/model-catalog"

/**
 * Return whether at least one selectable live catalog row exists.
 *
 * This controls model-picker usability. It does not claim endpoint health.
 */
export function useConnected() {
  const sync = useSync()
  return createMemo(() => hasLiveCatalogModels(sync.data.provider))
}
