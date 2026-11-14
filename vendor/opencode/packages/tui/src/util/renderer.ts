import { RendererControlState, type CliRenderer } from "@opentui/core"

type TitleRenderer = Pick<CliRenderer, "isDestroyed" | "controlState" | "setTerminalTitle">

// OpenTUI's native title control accepts printable text only: a C0/DEL/C1
// character makes it fail with InvalidArgument, and a session title carrying a
// newline (Ante's own title can be a slice of the first prompt) used to take
// the whole session down from inside a Solid effect.
const titleControls = /[\u0000-\u001f\u007f-\u009f]+/g

// OpenTUI rejects terminal control from suspend() until resume() completes, e.g. while $EDITOR runs.
export function setTerminalTitle(renderer: TitleRenderer, title: string) {
  if (renderer.isDestroyed || renderer.controlState === RendererControlState.EXPLICIT_SUSPENDED) return
  try {
    renderer.setTerminalTitle(title.replace(titleControls, " "))
  } catch {
    // The window title is decoration; never trade the session for it.
  }
}

export function destroyRenderer(renderer: TitleRenderer & Pick<CliRenderer, "destroy">) {
  if (renderer.isDestroyed) return
  setTerminalTitle(renderer, "")
  renderer.destroy()
}
