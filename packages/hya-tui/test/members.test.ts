import { expect, test } from "bun:test"
import type { MemberInfo, MessageInfo } from "../src/client"
import { childActivity, childSessionIds, childStatus, foldMember, taskLink } from "../src/state/members"

const spawn: MemberInfo = { member: "mbr_1", child: "hysec_c", agent: "scout", description: "survey", status: "MEMBER_STATUS_SPAWNING", callId: "call_1", depth: 1 }

test("member frames fold by member id without clearing known fields", () => {
  let rows = foldMember([], spawn)
  rows = foldMember(rows, { member: "mbr_1", status: "MEMBER_STATUS_RUNNING" })
  expect(rows).toEqual([{ ...spawn, status: "MEMBER_STATUS_RUNNING" }])
  rows = foldMember(rows, { member: "mbr_1", status: "MEMBER_STATUS_DONE", summary: "found 3 crates", child: "hysec_c" })
  expect(rows[0]).toMatchObject({ agent: "scout", callId: "call_1", status: "MEMBER_STATUS_DONE", summary: "found 3 crates" })
  rows = foldMember(rows, { member: "mbr_2", child: "hysec_d", agent: "general" })
  expect(rows.map((row) => row.member)).toEqual(["mbr_1", "mbr_2"])
})

test("a task card links to its member by call id, else by the child session in its output", () => {
  const members = [spawn, { member: "mbr_2", child: "hysec_d", agent: "general", status: "MEMBER_STATUS_SPAWNING" }]
  expect(taskLink({ callId: "call_1" }, members)).toEqual({ child: "hysec_c", member: spawn })
  // Resident spawns may leave `callId` empty: the task output's `metadata.sessionId` links them.
  expect(taskLink({ callId: "call_9", child: "hysec_d" }, members)).toEqual({ child: "hysec_d", member: members[1] })
  expect(taskLink({ callId: "call_9" }, members)).toEqual({})
  expect(taskLink({ child: "hysec_x" }, members)).toEqual({ child: "hysec_x" })
})

test("the child status prefers a finished member, then the child session's busy flag", () => {
  expect(childStatus(undefined, undefined)).toBe("starting")
  expect(childStatus(spawn, undefined)).toBe("starting")
  expect(childStatus({ ...spawn, status: "MEMBER_STATUS_RUNNING" }, undefined)).toBe("running")
  expect(childStatus(spawn, { busy: true })).toBe("running")
  expect(childStatus(spawn, { busy: false })).toBe("idle")
  expect(childStatus(spawn, { busy: false, failed: true })).toBe("failed")
  expect(childStatus({ ...spawn, status: "MEMBER_STATUS_DONE" }, { busy: true })).toBe("done")
  expect(childStatus({ ...spawn, status: "MEMBER_STATUS_FAILED" }, undefined)).toBe("failed")
  expect(childStatus({ ...spawn, status: "MEMBER_STATUS_CANCELLED" }, undefined)).toBe("cancelled")
})

test("the child's latest activity is its newest tool call or text line", () => {
  const messages: MessageInfo[] = [
    { id: "u", role: "ROLE_USER", parts: [{ id: "p", text: { text: "list files" } }] },
    { id: "a", role: "ROLE_ASSISTANT", parts: [
      { id: "t", toolCall: { tool: "read", state: "TOOL_EXECUTION_STATE_OK", inputJson: "{\"path\":\"notes.txt\"}" } },
    ] },
  ]
  expect(childActivity(messages)).toBe("read notes.txt")
  messages[1]!.parts!.push({ id: "x", text: { text: "\nFound three crates.\nMore detail" } })
  expect(childActivity(messages)).toBe("Found three crates.")
  expect(childActivity([])).toBeUndefined()
})

test("child sessions come from members and task outputs, once each", () => {
  const messages: MessageInfo[] = [{ id: "a", role: "ROLE_ASSISTANT", parts: [
    { id: "t", toolCall: { tool: "task", state: "TOOL_EXECUTION_STATE_OK", outputJson: "{\"metadata\":{\"sessionId\":\"hysec_e\"}}" } },
    { id: "t2", toolCall: { tool: "task", state: "TOOL_EXECUTION_STATE_OK", outputJson: "{\"metadata\":{\"sessionId\":\"hysec_c\"}}" } },
  ] }]
  expect(childSessionIds([spawn], messages)).toEqual(["hysec_c", "hysec_e"])
})
