import { expect, test } from "bun:test"
import { RendererControlState } from "@opentui/core"
import { createTestRenderer } from "@opentui/core/testing"
import { destroyRenderer, setTerminalTitle } from "../../src/util/renderer"

test("sets a title that contains control characters", async () => {
  await using setup = await createTestRenderer({ width: 10, height: 2 })
  await setup.renderer.setupTerminal()
  expect(() => setTerminalTitle(setup.renderer, "Fix parser bug\nin tokenizer")).not.toThrow()
})

test("clears the terminal title before destroying the renderer", () => {
  const calls: string[] = []
  destroyRenderer({
    isDestroyed: false,
    controlState: RendererControlState.IDLE,
    setTerminalTitle(title) {
      calls.push(`title:${title}`)
    },
    destroy() {
      calls.push("destroy")
    },
  })
  expect(calls).toEqual(["title:", "destroy"])
})

test("flattens control characters the native title control rejects", () => {
  // 2026-11-14: Ante titled a session with the first prompt, newlines included.
  // OpenTUI's native control validated the text, threw InvalidArgument from the
  // title effect, and the session died before the first reply landed.
  const calls: string[] = []
  setTerminalTitle(
    {
      isDestroyed: false,
      controlState: RendererControlState.IDLE,
      setTerminalTitle(title) {
        calls.push(title)
      },
    },
    "OC | #工作报告\n\n天河\t私厨",
  )
  expect(calls).toEqual(["OC | #工作报告 天河 私厨"])
})

test("survives a title the native control still refuses", () => {
  expect(() =>
    setTerminalTitle(
      {
        isDestroyed: false,
        controlState: RendererControlState.IDLE,
        setTerminalTitle() {
          throw new Error("ot_session_control failed: InvalidArgument")
        },
      },
      "OpenCode",
    ),
  ).not.toThrow()
})

test("skips the title from suspend until resume completes and destroys a suspended renderer", async () => {
  await using setup = await createTestRenderer({ width: 10, height: 2 })
  await setup.renderer.setupTerminal()
  const suspending = setup.renderer.suspend()
  setTerminalTitle(setup.renderer, "suspending")
  await suspending
  const resuming = setup.renderer.resume()
  setTerminalTitle(setup.renderer, "resuming")
  await resuming
  await setup.renderer.suspend()
  destroyRenderer(setup.renderer)
  expect(setup.renderer.isDestroyed).toBe(true)
  expect(() => destroyRenderer(setup.renderer)).not.toThrow()
  await setup.renderer.closed
  expect(() => destroyRenderer(setup.renderer)).not.toThrow()
})
