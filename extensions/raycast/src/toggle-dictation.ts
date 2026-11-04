import { closeMainWindow, showToast, Toast } from "@raycast/api";
import { toggleDictation } from "./dictation-control";

export default async function ToggleDictation() {
  try {
    // Restore focus first: Stenly should paste into the original app, not the launcher.
    await closeMainWindow({ clearRootSearch: true });
    await toggleDictation();
  } catch (error) {
    await showToast({
      style: Toast.Style.Failure,
      title: "Couldn't toggle dictation",
      message: error instanceof Error ? error.message : "Open Stenly and try again.",
    });
  }
}
