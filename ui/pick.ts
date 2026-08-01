/** Native file/folder pickers.
 *
 * Wrapped so components never import the plugin directly, and so a cancelled
 * dialog is a plain `null` rather than three different shapes depending on
 * whether the user picked one path, several, or none. */

import { open } from "@tauri-apps/plugin-dialog";

/** Pick a directory. Returns null if the user cancelled. */
export async function pickDirectory(
  title: string,
  defaultPath?: string | null,
): Promise<string | null> {
  const picked = await open({
    directory: true,
    multiple: false,
    title,
    // Reopening where the user last was beats making them navigate from their
    // home directory every time.
    defaultPath: defaultPath || undefined,
  });
  // The plugin returns string | string[] | null depending on `multiple`; with
  // multiple:false only the first two are reachable, but narrowing here keeps
  // callers from having to care.
  if (picked == null) return null;
  return Array.isArray(picked) ? (picked[0] ?? null) : picked;
}

/** Pick a single executable. Returns null if the user cancelled. */
export async function pickExecutable(
  title: string,
  defaultPath?: string | null,
): Promise<string | null> {
  const picked = await open({
    directory: false,
    multiple: false,
    title,
    defaultPath: defaultPath || undefined,
    // Windows hides extensions and shows a great many files; naming the filter
    // makes the binary findable. Other platforms have no extension to filter
    // on, so they get the unrestricted entry.
    filters: [
      { name: "Executable", extensions: ["exe"] },
      { name: "All files", extensions: ["*"] },
    ],
  });
  if (picked == null) return null;
  return Array.isArray(picked) ? (picked[0] ?? null) : picked;
}
