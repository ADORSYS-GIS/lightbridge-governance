// The test stub exposes one thing `@types/vscode` does not: a settings bag the
// suite writes before each scenario, standing in for
// `workspace.getConfiguration`. Declared as an augmentation so the tests stay
// fully typed against the real `vscode` types everywhere else — the stub is a
// runtime substitution only, never a type-level one.
declare module 'vscode' {
  export const __settings: Record<string, unknown>;

  /** What the stub recorded of the UI the extension asked for. */
  export const __ui: {
    errorMessages: Array<{ message: string; actions: string[] }>;
    errorMessageResponse: string | undefined;
    terminals: Array<{ name: string; shown: boolean; sent: Array<{ text: string; execute: boolean }> }>;
    statusBar: { visible: boolean; text: string; tooltip: string };
  };
}

export {};
