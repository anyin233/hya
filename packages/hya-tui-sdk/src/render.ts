import type { TranscriptNodeProps, TranscriptRenderNode } from "./wire";

export {
  RENDER_LIMITS, isTranscriptNode, validateTranscriptNode,
  type NodeAction, type RenderNode, type TextStyle, type TranscriptNodeProps, type TranscriptRenderNode,
} from "./wire";

/** Construct a host-virtualized streaming transcript node (`{ type: "transcript", sessionId, ... }`). */
export function transcriptNode(
  sessionId: string,
  options?: Omit<TranscriptNodeProps, "sessionId">,
): TranscriptRenderNode {
  return {
    type: "transcript",
    sessionId,
    ...(options?.follow !== undefined ? { follow: options.follow } : {}),
    ...(options?.scrollOffset !== undefined ? { scrollOffset: options.scrollOffset } : {}),
    ...(options?.maxMessages !== undefined ? { maxMessages: options.maxMessages } : {}),
    ...(options?.compact !== undefined ? { compact: options.compact } : {}),
  };
}
